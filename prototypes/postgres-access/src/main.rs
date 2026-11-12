//! Spike: what Postgres sees when a User reaches a database through an
//! Application. Usage: pg-access-spike <port> (a fresh cluster whose superuser
//! is postgres/spike). Prints one line per check and exits non-zero if any
//! check's observation differs from what the recommended design expects.

mod host;
mod policy;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use host::*;
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, Client, NoTls};

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Blocked,
    Allowed,
    Info,
}

struct Rec {
    section: &'static str,
    name: String,
    expect: Expect,
    blocked: bool,
    detail: String,
}

struct Env {
    port: u16,
    su_shared: Client,
    su_app: Client,
    read: Arc<Pool>,
    write: Arc<Pool>,
    structp: Arc<Pool>,
    rt: Arc<Pool>,
    notes: mpsc::UnboundedReceiver<String>,
    secret: String,
    recs: Vec<Rec>,
}

fn short(s: &str) -> String {
    let s = s.replace('\n', " ");
    if s.len() > 150 { format!("{}...", &s[..150]) } else { s }
}

fn res_str(r: &Result<Rows, HostError>) -> String {
    match r {
        Ok(rows) => {
            let first: Vec<String> = rows.first().map(|r| r.iter().map(|v| v.text()).collect()).unwrap_or_default();
            short(&format!("ok, {} row(s) {:?}", rows.len(), first))
        }
        Err(e) => short(&format!("error: {e}")),
    }
}

async fn su(port: u16, db: &str) -> Client {
    ConnSpec::new(port, db, "postgres", "spike").connect().await.expect("superuser connect").client
}

fn cfg(parser: bool, ro: bool, discard: bool, identity: Identity) -> HostCfg {
    HostCfg { parser, read_only_txn: ro, discard, identity, limits: Limits::default() }
}

fn alice() -> Ctx {
    Ctx::new("alice@corp", "app1")
}
fn bob() -> Ctx {
    Ctx::new("bob@corp", "app1")
}

#[derive(Clone, Copy)]
enum Probe {
    /// Blocked iff the op errored.
    Err,
    /// Blocked iff hr.emp salaries did not change.
    Salary,
    /// Blocked iff IT's listener did not receive a notification.
    Notify,
    /// Blocked iff no new large object exists.
    LargeObject,
    /// Blocked iff the op errored or returned no rows.
    Rows,
}

impl Env {
    fn rec(&mut self, section: &'static str, name: impl Into<String>, expect: Expect, blocked: bool, detail: impl Into<String>) {
        self.recs.push(Rec { section, name: name.into(), expect, blocked, detail: detail.into() });
    }

    async fn salary_sum(&self) -> i64 {
        self.su_shared.query_one("select sum(salary)::int8 from hr.emp", &[]).await.unwrap().get(0)
    }
    async fn reset_salary(&self) {
        self.su_shared
            .batch_execute("update hr.emp set salary = case id when 1 then 100 when 2 then 200 else 900 end")
            .await
            .unwrap();
    }
    async fn lo_count(&self) -> i64 {
        self.su_shared.query_one("select count(*) from pg_largeobject_metadata", &[]).await.unwrap().get(0)
    }
    async fn drain_notes(&mut self) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut v = Vec::new();
        while let Ok(n) = self.notes.try_recv() {
            v.push(n);
        }
        v
    }

    /// Run one op and evaluate the probe. Returns (blocked, detail).
    async fn probe_op(&mut self, pool: &Arc<Pool>, c: &HostCfg, level: Level, sql: &str, probe: Probe) -> (bool, String) {
        let s0 = self.salary_sum().await;
        let l0 = self.lo_count().await;
        self.drain_notes().await;
        let r = run_op(pool, c, &alice(), level, sql, &[]).await;
        let blocked = match probe {
            Probe::Err => r.is_err(),
            Probe::Rows => r.as_ref().map(|x| x.is_empty()).unwrap_or(true),
            Probe::Salary => self.salary_sum().await == s0,
            Probe::Notify => self.drain_notes().await.is_empty(),
            Probe::LargeObject => self.lo_count().await == l0,
        };
        self.reset_salary().await;
        (blocked, res_str(&r))
    }

    /// Same attack under three host configurations, to show which layer holds.
    async fn matrix(&mut self, name: &str, sql: &str, probe: Probe, expect: Expect) {
        let layers = [
            ("privileges only", cfg(false, false, true, Identity::Label)),
            ("+READ ONLY txn", cfg(false, true, true, Identity::Label)),
            ("+parser (full)", cfg(true, true, true, Identity::Label)),
        ];
        let mut parts = Vec::new();
        let mut last = (false, String::new());
        let pool = self.read.clone();
        for (lname, c) in layers.iter() {
            let (b, d) = self.probe_op(&pool, c, Level::Read, sql, probe).await;
            parts.push(format!("{lname}={}", if b { "BLOCKED" } else { "ALLOWED" }));
            last = (b, d);
        }
        let detail = format!("[{}] full: {}", parts.join(", "), last.1);
        self.rec("1 access levels (read login)", name, expect, last.0, detail);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args().nth(1).expect("port").parse()?;
    let only_bench = std::env::args().any(|a| a == "--bench-only");
    setup(port).await?;

    // IT's own listener on shared_db (kept alive for the whole run).
    let (_lclient, lnotes) = listener(port).await?;
    let secret = setup_secret();

    let mut env = Env {
        port,
        su_shared: su(port, "shared_db").await,
        su_app: su(port, "app1_db").await,
        read: Pool::new(ConnSpec::new(port, "shared_db", "vm_read", "read")),
        write: Pool::new(ConnSpec::new(port, "shared_db", "vm_write", "write")),
        structp: Pool::new(ConnSpec::new(port, "shared_db", "vm_struct", "struct")),
        rt: Pool::new(ConnSpec::new(port, "app1_db", "app1_rt", "rt")),
        notes: lnotes,
        secret,
        recs: Vec::new(),
    };

    let version: String = env.su_shared.query_one("select version()", &[]).await?.get(0);
    println!("# {version}");

    if !only_bench {
        s1_access(&mut env).await;
        s1_roles(&mut env).await;
        s2_identity(&mut env).await;
        s3_trigger(&mut env).await;
        s4_transactions(&mut env).await;
        s5_limits(&mut env).await;
        s5_session_leaks(&mut env).await;
        s6_structure(&mut env).await;
    }

    let mut fails = 0;
    let mut section = "";
    for r in &env.recs {
        if r.section != section {
            section = r.section;
            println!("\n## {section}");
        }
        let obs = if r.blocked { "BLOCKED" } else { "ALLOWED" };
        let (verdict, exp) = match r.expect {
            Expect::Blocked => (if r.blocked { "PASS" } else { "FAIL" }, "expect BLOCKED"),
            Expect::Allowed => (if r.blocked { "FAIL" } else { "PASS" }, "expect ALLOWED"),
            Expect::Info => ("INFO", "observation  "),
        };
        if verdict == "FAIL" {
            fails += 1;
        }
        println!("{verdict}  {exp}  observed {obs}  {}\n        {}", r.name, r.detail);
    }

    bench(&mut env).await;
    println!("\n# checks: {} total, {} FAIL", env.recs.len(), fails);
    std::process::exit(if fails == 0 { 0 } else { 1 });
}

async fn listener(port: u16) -> anyhow::Result<(Client, mpsc::UnboundedReceiver<String>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let (client, mut conn) = tokio_postgres::connect(
        &format!("host=127.0.0.1 port={port} dbname=shared_db user=it_owner password=it"),
        NoTls,
    )
    .await?;
    tokio::spawn(async move {
        let mut s = futures_util::stream::poll_fn(move |cx| conn.poll_message(cx));
        while let Some(Ok(m)) = s.next().await {
            if let AsyncMessage::Notification(n) = m {
                let _ = tx.send(n.payload().to_string());
            }
        }
    });
    client.batch_execute("LISTEN hr_events").await?;
    Ok((client, rx))
}

// ------------------------------------------------------------------- setup

/// The attach secret lives only in host memory (generated per run).
static SECRET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
fn setup_secret() -> String {
    SECRET.get().cloned().expect("setup ran")
}

async fn setup(port: u16) -> anyhow::Result<()> {
    let root = env!("CARGO_MANIFEST_DIR");
    let read = |f: &str| std::fs::read_to_string(format!("{root}/sql/{f}")).unwrap();
    // CREATE DATABASE cannot run in the implicit transaction of a multi-statement string.
    let s = su(port, "postgres").await;
    for stmt in read("cluster.sql").split(';').filter(|x| x.lines().any(|l| !l.trim().is_empty() && !l.trim_start().starts_with("--"))) {
        s.batch_execute(stmt).await?;
    }
    ConnSpec::new(port, "shared_db", "it_owner", "it").connect().await?.client.batch_execute(&read("shared_db.sql")).await?;
    let mig = ConnSpec::new(port, "app1_db", "app1_migrator", "migrator").connect().await?.client;
    mig.batch_execute(&read("app_db.sql")).await?;
    // Random keys; the host keeps the plain attach secret.
    let secret = hex::encode(rand::random::<[u8; 32]>());
    let k1 = rand::random::<[u8; 32]>().to_vec();
    let k2 = rand::random::<[u8; 32]>().to_vec();
    mig.execute(
        "insert into vm.keys values (sha256(convert_to($1::text,'UTF8')), $2, $3)",
        &[&secret, &k1, &k2],
    )
    .await?;
    SECRET.set(secret).ok();
    // Superuser-only fixtures.
    let s = su(port, "app1_db").await;
    s.batch_execute(
        "create table app.rt_owned (id int primary key, v int);
         create trigger rt_owned_history after insert or update or delete on app.rt_owned
           for each row execute function vm.capture();
         alter table app.rt_owned owner to app1_rt;
         create schema sandwich; grant usage on schema sandwich to host_login;
         create function sandwich.run_as_app(q text) returns void language plpgsql security definer
           as $$ begin execute q; end $$;
         alter function sandwich.run_as_app(text) owner to app1_rt;
         grant execute on function sandwich.run_as_app(text) to host_login;",
    )
    .await?;
    su(port, "shared_db").await.batch_execute("analyze hr.emp").await?;
    Ok(())
}

// ------------------------------------------------------- 1. access levels

async fn s1_access(env: &mut Env) {
    use Expect::*;
    use Probe::*;
    env.matrix("UPDATE through the read login", "update hr.emp set salary = salary + 1", Salary, Blocked).await;
    env.matrix(
        "data-modifying CTE",
        "with u as (update hr.emp set salary = salary + 1 returning id) select count(*) from u",
        Salary,
        Blocked,
    )
    .await;
    env.matrix("SECURITY DEFINER function with a side effect (IT's hr.give_raise)", "select hr.give_raise()", Salary, Blocked).await;
    env.matrix(
        "query_to_xml runs SQL text the parser cannot see",
        "select query_to_xml('select hr.give_raise()', true, true, '')",
        Salary,
        Blocked,
    )
    .await;
    env.matrix(
        "ts_stat runs SQL text (not on the parser deny-list)",
        "select * from ts_stat('select to_tsvector(hr.give_raise()::text)')",
        Salary,
        Blocked,
    )
    .await;
    env.matrix("SELECT ... FOR UPDATE", "select * from hr.emp for update", Err, Blocked).await;
    env.matrix("COPY hr.emp TO STDOUT", "copy hr.emp to stdout", Err, Blocked).await;
    env.matrix("DO block calling a side-effect function", "do $$ begin perform hr.give_raise(); end $$", Salary, Blocked).await;
    env.matrix("CREATE TEMP TABLE", "create temp table t_app (a int)", Err, Blocked).await;
    env.matrix("CREATE FUNCTION in pg_temp", "create function pg_temp.f() returns int language sql as 'select 1'", Err, Blocked).await;
    env.matrix("NOTIFY to a channel IT listens on", "notify hr_events, 'from-app'", Notify, Blocked).await;
    env.matrix("pg_notify() to a channel IT listens on", "select pg_notify('hr_events', 'from-app-fn')", Notify, Blocked).await;
    env.matrix(
        "pg_notify() hidden in ts_stat (not on deny-list)",
        "select * from ts_stat($q$select to_tsvector(pg_notify('hr_events','hidden')::text)$q$)",
        Notify,
        Info,
    )
    .await;
    env.matrix(
        "pg_terminate_backend hidden in ts_stat (not on deny-list; probe: op errors)",
        "select * from ts_stat($q$select to_tsvector(pg_terminate_backend(0)::text)$q$)",
        Err,
        Info,
    )
    .await;
    env.matrix("LISTEN (session state on a pooled connection)", "listen anything", Err, Blocked).await;
    env.matrix("SET ROLE to IT's owner role", "set role it_owner", Err, Blocked).await;
    env.matrix("SET SESSION AUTHORIZATION", "set session authorization it_owner", Err, Blocked).await;
    env.matrix("set_config('role', ...)", "select set_config('role', 'it_owner', false)", Err, Blocked).await;
    env.matrix("large object create (lo_from_bytea)", "select lo_from_bytea(0, 'x')", LargeObject, Blocked).await;
    env.matrix("read pg_authid", "select rolname, rolpassword from pg_authid", Err, Blocked).await;
    env.matrix("read a schema not granted (it_private.secret)", "select * from it_private.secret", Err, Blocked).await;
    env.matrix(
        "set_config('transaction_read_only','off') then write",
        "select set_config('transaction_read_only', 'off', true), hr.give_raise()",
        Salary,
        Blocked,
    )
    .await;
    env.matrix("two statements in one string", "select 1; select hr.give_raise()", Salary, Blocked).await;
    env.matrix(
        "catalogs list every table, incl. ungranted schemas",
        "select relname from pg_class where relnamespace = 'it_private'::regnamespace",
        Rows,
        Allowed,
    )
    .await;
    env.matrix("pg_sleep is callable (bounded by the host timeout, section 5)", "select pg_sleep(0.01)", Err, Allowed).await;

    // Multi-statement strings over the simple protocol (the host never uses it
    // for Application SQL): shows why.
    {
        let c = env.read.spec.connect().await.unwrap();
        let s0 = env.salary_sum().await;
        let r = c.client.simple_query("select 1; select hr.give_raise()").await;
        let changed = env.salary_sum().await != s0;
        env.reset_salary().await;
        env.rec(
            "1 access levels (read login)",
            "simple protocol runs both statements of 'select 1; select hr.give_raise()' (no READ ONLY)",
            Info,
            !changed,
            format!("{:?}", r.map(|_| "ok").map_err(|e| pg_err(e).to_string())),
        );
    }

    // Victim backends on the same login role.
    {
        let victim = env.read.spec.connect().await.unwrap();
        let vh = tokio::spawn(async move {
            victim.client.simple_query("select pg_sleep(2), 'victim-literal-42'").await.map(|_| ()).map_err(|e| pg_err(e).to_string())
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        for (name, full) in [("privileges only", false), ("+parser (full)", true)] {
            let c = cfg(full, true, true, Identity::Label);
            let r = run_op(
                &env.read,
                &c,
                &alice(),
                Level::Read,
                "select query from pg_stat_activity where query like '%victim-literal%' and pid <> pg_backend_pid()",
                &[],
            )
            .await;
            let seen = r.as_ref().map(|x| !x.is_empty()).unwrap_or(false);
            env.rec(
                "1 access levels (read login)",
                format!("pg_stat_activity shows another backend's SQL text on the same login [{name}]"),
                if full { Blocked } else { Info },
                !seen,
                res_str(&r),
            );
        }
        let _ = vh.await;
    }
    for (name, full) in [("privileges only", false), ("+parser (full)", true)] {
        let victim = env.read.spec.connect().await.unwrap();
        let vpid = victim.pid;
        let vh = tokio::spawn(async move { victim.client.simple_query("select pg_sleep(2)").await.map(|_| ()).map_err(|e| pg_err(e).to_string()) });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let c = cfg(full, true, true, Identity::Label);
        let r = run_op(&env.read, &c, &alice(), Level::Read, &format!("select pg_terminate_backend({vpid})"), &[]).await;
        let vr = vh.await.unwrap();
        env.rec(
            "1 access levels (read login)",
            format!("pg_terminate_backend on another request's backend (same login) [{name}]"),
            if full { Blocked } else { Info },
            vr.is_ok(),
            format!("op: {} | victim: {:?}", res_str(&r), vr),
        );
    }

    // Write login: what privileges stop without any parser.
    let c = cfg(false, false, true, Identity::Label);
    for (name, sql, exp) in [
        ("write login: UPDATE (granted)", "update hr.emp set status = status", Allowed),
        ("write login: data-modifying CTE (granted)", "with u as (update hr.emp set status = status returning id) select count(*) from u", Allowed),
        ("write login: TRUNCATE", "truncate hr.emp", Blocked),
        ("write login: ALTER TABLE", "alter table hr.emp add column x int", Blocked),
        ("write login: DROP TABLE", "drop table hr.dept", Blocked),
        ("write login: CREATE TABLE in hr", "create table hr.x (a int)", Blocked),
        ("write login: CREATE TABLE in public", "create table public.x (a int)", Blocked),
        ("write login: CREATE TEMP TABLE (TEMP is PUBLIC by default)", "create temp table t (a int)", Info),
    ] {
        let r = run_op(&env.write, &c, &alice(), Level::Write, sql, &[]).await;
        env.rec("1 access levels (write login, privileges only)", name, exp, r.is_err(), res_str(&r));
    }
    {
        // GRANT without grant option succeeds with a WARNING and grants nothing.
        let r = run_op(&env.write, &c, &alice(), Level::Write, "grant select on hr.emp to public", &[]).await;
        let public: bool = env.su_shared.query_one("select has_table_privilege('public', 'hr.emp', 'select')", &[]).await.unwrap().get(0);
        env.rec("1 access levels (write login, privileges only)", "write login: GRANT SELECT ON hr.emp TO PUBLIC", Expect::Blocked, !public, format!("{} (PUBLIC can now select: {public})", res_str(&r)));
    }
}

async fn s1_roles(env: &mut Env) {
    use Expect::*;
    let sec = "1 roles: can Application SQL get back to a more powerful role?";
    // Login directly as the low role: RESET ROLE has nowhere to go.
    {
        let c = cfg(false, true, true, Identity::Label);
        let tx = begin(&env.read, &c, &alice(), Level::Read).await.unwrap();
        let a = tx.exec("reset role").await;
        let b = tx.exec("select current_user::text, session_user::text").await;
        let who = b.as_ref().ok().and_then(|r| r.first()).map(|r| r[0].text()).unwrap_or_default();
        env.rec(sec, "login directly as vm_read; App runs RESET ROLE", Blocked, who == "vm_read", format!("{} then {}", res_str(&a), res_str(&b)));
        let _ = tx.commit().await;
    }
    // Rejected design: host_login + SET ROLE app1_rt.
    let hl = ConnSpec::new(env.port, "app1_db", "host_login", "host").connect().await.unwrap();
    hl.client.batch_execute("set role app1_rt").await.unwrap();
    let before = hl.client.simple_query("select count(*) from vm.keys").await;
    let _ = hl.client.batch_execute("reset role").await;
    let after = hl.client.simple_query("select count(*) from vm.keys").await;
    env.rec(
        sec,
        "one host login + SET ROLE app1_rt; App runs RESET ROLE, then reads vm.keys",
        Info,
        after.is_err(),
        format!("before reset: {:?}; after reset: {:?}", before.map(|_| "ok").map_err(|e| pg_err(e).to_string()), after.map(|_| "ok").map_err(|e| pg_err(e).to_string())),
    );
    // Same, with the parser in front: the parser blocks RESET ROLE and
    // set_config, but not SQL text run by ts_stat.
    hl.client.batch_execute("set role app1_rt").await.unwrap();
    let p1 = policy::check("reset role", Level::Read).is_err();
    let trick = "select * from ts_stat($q$select to_tsvector(set_config('role','none',false))$q$)";
    let p2 = policy::check(trick, Level::Read);
    let _ = hl.client.simple_query(trick).await;
    let who: String = hl.client.query_one("select current_user::text", &[]).await.unwrap().get(0);
    env.rec(
        sec,
        "one host login + SET ROLE + parser: RESET ROLE refused, but ts_stat(set_config('role','none')) passes the parser",
        Info,
        who == "app1_rt",
        format!("parser refuses RESET ROLE: {p1}; parser on trick: {:?}; current_user after trick: {who}", p2),
    );
    // SECURITY DEFINER sandwich: App SQL executed inside a function owned by
    // app1_rt cannot change role.
    let r = hl.client.simple_query("reset role; select sandwich.run_as_app('reset role')").await;
    let r2 = hl.client.simple_query("select sandwich.run_as_app($q$select set_config('role','none',false)$q$)").await;
    env.rec(
        sec,
        "App SQL run inside a SECURITY DEFINER function owned by app1_rt: RESET ROLE / set_config('role')",
        Info,
        r.is_err() && r2.is_err(),
        format!("{:?} / {:?}", r.map(|_| "ok").map_err(|e| pg_err(e).to_string()), r2.map(|_| "ok").map_err(|e| pg_err(e).to_string())),
    );
}

// --------------------------------------------------------------- 2. identity

fn signed(env: &Env, parser: bool) -> HostCfg {
    cfg(parser, false, true, Identity::Signed { secret: env.secret.clone() })
}

async fn history_count(env: &Env, user: &str) -> i64 {
    env.su_app.query_one("select count(*) from vm.history where user_id = $1", &[&user]).await.unwrap().get(0)
}

async fn s2_identity(env: &mut Env) {
    use Expect::*;
    let sec = "2 identity in the session";

    // A: plain custom settings (all that is possible with nothing installed).
    {
        let c = cfg(false, true, true, Identity::Label);
        let tx = begin(&env.read, &c, &alice(), Level::Read).await.unwrap();
        let _ = tx.exec("select set_config('vm.user', 'mallory', true)").await;
        let r = tx.exec("select current_setting('vm.user')").await;
        let v = r.as_ref().ok().and_then(|r| r.first()).map(|r| r[0].text()).unwrap_or_default();
        env.rec(sec, "A plain setting vm.user (nothing installed): App overwrites it", Info, v != "mallory", format!("vm.user now = {v}"));
        let _ = tx.commit().await;
    }
    // GRANT SET ON PARAMETER does not protect a custom placeholder.
    {
        let gr = env.su_app.batch_execute("grant set on parameter vm.locked to app1_migrator").await;
        let c = env.rt.spec.connect().await.unwrap();
        let r = c.client.simple_query("select set_config('vm.locked', 'x', false)").await;
        env.rec(
            sec,
            "GRANT SET ON PARAMETER vm.locked to another role; app1_rt still sets vm.locked",
            Info,
            r.is_err(),
            format!("grant: {:?}; set: {:?}", gr.map_err(|e| pg_err(e).to_string()), r.map(|_| "ok").map_err(|e| pg_err(e).to_string())),
        );
    }

    // B: signed, transaction-bound identity. Parser OFF throughout: the
    // database must hold on its own.
    let c = signed(env, false);
    {
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "select (vm.identity()).*", &[]).await;
        let ok = r.as_ref().ok().and_then(|r| r.first()).map(|r| r[0].text() == "alice@corp").unwrap_or(false);
        env.rec(sec, "B signed: host attaches alice, vm.identity() returns alice", Allowed, !ok, res_str(&r));
    }
    {
        let tx = begin(&env.rt, &c, &alice(), Level::Write).await.unwrap();
        let _ = tx.exec(r#"select set_config('vm.ctx', '{"u":"mallory@corp","a":"app1","t":"x","s":"00"}', true)"#).await;
        let r = tx.exec("insert into app.orders (item, qty) values ('forged', 1)").await;
        drop(tx);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let n = history_count(env, "mallory@corp").await;
        env.rec(sec, "B signed: App overwrites vm.ctx with a forged value, then writes", Blocked, r.is_err() && n == 0, res_str(&r));
    }
    {
        // Capture alice's value in one transaction, replay it in bob's.
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "select current_setting('vm.ctx')", &[]).await;
        let captured = r.ok().and_then(|r| r.first().map(|x| x[0].text())).unwrap_or_default();
        let tx = begin(&env.rt, &c, &bob(), Level::Write).await.unwrap();
        let set = format!("select set_config('vm.ctx', '{}', true)", captured.replace('\'', "''"));
        let _ = tx.exec(&set).await;
        let w = tx.exec("insert into app.orders (item, qty) values ('replayed', 1)").await;
        let _ = tx.commit().await;
        let n: i64 = env.su_app.query_one("select count(*) from vm.history where new_row->>'item' = 'replayed'", &[]).await.unwrap().get(0);
        env.rec(sec, "B signed: App replays alice's vm.ctx (captured earlier) inside bob's transaction", Blocked, w.is_err() && n == 0, res_str(&w));
    }
    {
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "select vm.attach('mallory@corp', 'app1', 'guess')", &[]).await;
        env.rec(sec, "B signed: App calls vm.attach with a guessed secret", Blocked, r.is_err(), res_str(&r));
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "select vm._tag('[\"mallory\",\"app1\",\"x\"]')", &[]).await;
        env.rec(sec, "B signed: App calls the signing function vm._tag", Blocked, r.is_err(), res_str(&r));
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "select * from vm.keys", &[]).await;
        env.rec(sec, "B signed: App reads vm.keys", Blocked, r.is_err(), res_str(&r));
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "create temp table x (a int)", &[]).await;
        env.rec(sec, "B signed: App creates a temp table / pg_temp shadow (no TEMP on app DB)", Blocked, r.is_err(), res_str(&r));
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "select prosrc from pg_proc where proname = 'attach'", &[]).await;
        env.rec(sec, "B signed: App reads vm.attach's source (readable; holds no key)", Info, r.is_err(), res_str(&r));
    }
    {
        // Can the secret be observed in pg_stat_activity while the host attaches?
        let spy = env.rt.spec.connect().await.unwrap();
        let secret = env.secret.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let st = stop.clone();
        let spyh = tokio::spawn(async move {
            let mut seen = 0usize;
            let mut texts = std::collections::BTreeSet::new();
            while !st.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(rows) = spy.client.query("select query from pg_stat_activity where usename = 'app1_rt' and pid <> pg_backend_pid()", &[]).await {
                    for r in rows {
                        let q: Option<String> = r.get(0);
                        let q = q.unwrap_or_default();
                        if q.contains(&secret) {
                            seen += 1;
                        }
                        texts.insert(q);
                    }
                }
            }
            (seen, texts)
        });
        for _ in 0..300 {
            let _ = run_op(&env.rt, &c, &alice(), Level::Write, "select 1", &[]).await;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (seen, texts) = spyh.await.unwrap();
        env.rec(
            sec,
            "B signed: App polls pg_stat_activity during 300 host attaches looking for the secret",
            Blocked,
            seen == 0,
            short(&format!("secret seen {seen} times; distinct texts seen: {texts:?}")),
        );
    }
    {
        // No host, no identity: direct connection as app1_rt writes.
        let d = env.rt.spec.connect().await.unwrap();
        let r = d.client.simple_query("insert into app.orders (item, qty) values ('no-identity', 1)").await;
        env.rec(sec, "B signed: write with no identity attached fails closed", Blocked, r.is_err(), format!("{:?}", r.map(|_| "ok").map_err(|e| pg_err(e).to_string())));
    }
    {
        // Two transactions in one simple-protocol message share transaction_timestamp():
        // why the transaction key also includes the virtual transaction id.
        let rows = env
            .su_app
            .simple_query("begin; select transaction_timestamp()::text, (select virtualtransaction from pg_locks where pid=pg_backend_pid() and locktype='virtualxid'); commit; begin; select transaction_timestamp()::text, (select virtualtransaction from pg_locks where pid=pg_backend_pid() and locktype='virtualxid'); commit;")
            .await
            .unwrap();
        let vals: Vec<String> = rows
            .iter()
            .filter_map(|m| if let tokio_postgres::SimpleQueryMessage::Row(r) = m { Some(format!("{}|{}", r.get(0).unwrap(), r.get(1).unwrap())) } else { None })
            .collect();
        let same_ts = vals.len() == 2 && vals[0].split('|').next() == vals[1].split('|').next();
        env.rec(sec, "two transactions in one simple-protocol message: same transaction_timestamp, different vxid", Info, !same_ts, format!("{vals:?}"));
    }

    // An RLS policy reads the same identity.
    {
        let r = run_op(&env.rt, &c, &alice(), Level::Write, "select body from app.notes order by id", &[]).await;
        let only_alice = r.as_ref().map(|x| x.len() == 1 && x[0][0].text() == "alice-note").unwrap_or(false);
        env.rec(sec, "B signed: RLS policy using vm.identity() shows alice only her row", Allowed, !only_alice, res_str(&r));
        let tx = begin(&env.rt, &c, &alice(), Level::Write).await.unwrap();
        let _ = tx.exec(r#"select set_config('vm.ctx', '{"u":"bob@corp","a":"app1","t":"x","s":"00"}', true)"#).await;
        let r = tx.exec("select body from app.notes").await;
        drop(tx);
        env.rec(sec, "B signed: forged vm.ctx then read the RLS table", Blocked, r.is_err(), res_str(&r));
    }

    // C: platform-owned table keyed by backend + transaction.
    {
        let cc = cfg(false, false, true, Identity::Table { secret: env.secret.clone() });
        let r = run_op(&env.rt, &cc, &alice(), Level::Write, "select (vm.identity_c()).*", &[]).await;
        env.rec(sec, "C table: host attaches alice via vm.attach_c, vm.identity_c() returns it", Allowed, r.is_err(), res_str(&r));
        let r = run_op(&env.rt, &cc, &alice(), Level::Write, "insert into vm.attached values (pg_backend_pid(), vm._txkey(), 'mallory', 'app1') on conflict (pid) do update set user_id = 'mallory'", &[]).await;
        env.rec(sec, "C table: App writes vm.attached directly", Blocked, r.is_err(), res_str(&r));
        let ro = cfg(false, true, true, Identity::Table { secret: env.secret.clone() });
        let r = run_op(&env.rt, &ro, &alice(), Level::Read, "select 1", &[]).await;
        env.rec(sec, "C table: attach inside a READ ONLY transaction (it is a write)", Info, r.is_err(), res_str(&r));
    }
}

// ------------------------------------------------------- 3. Change history

async fn s3_trigger(env: &mut Env) {
    use Expect::*;
    let sec = "3 Change history trigger (app DB, as app1_rt, parser off)";
    let c = signed(env, false);
    {
        let a = run_op(&env.rt, &c, &alice(), Level::Write, "insert into app.orders (item, qty) values ('widget', 2) returning id", &[]).await;
        let id = a.as_ref().ok().and_then(|r| r.first()).map(|r| r[0].text()).unwrap_or_default();
        let b = run_op(&env.rt, &c, &bob(), Level::Write, &format!("update app.orders set qty = 3, approved_by = 'bob' where id = {id}"), &[]).await;
        let d = run_op(&env.rt, &c, &alice(), Level::Write, &format!("delete from app.orders where id = {id}"), &[]).await;
        let rows = env
            .su_app
            .query(
                "select op, user_id, app_id, coalesce(old_row->>'qty','-'), coalesce(new_row->>'qty','-') from vm.history where coalesce(new_row, old_row)->>'id' = $1 order by id",
                &[&id],
            )
            .await
            .unwrap();
        let got: Vec<String> = rows
            .iter()
            .map(|r| format!("{} {} {} {}->{}", r.get::<_, String>(0), r.get::<_, String>(1), r.get::<_, String>(2), r.get::<_, String>(3), r.get::<_, String>(4)))
            .collect();
        let ok = a.is_ok() && b.is_ok() && d.is_ok() && got.len() == 3 && got[1].starts_with("UPDATE bob@corp app1 2->3");
        env.rec(sec, "insert (alice), update (bob), delete (alice) are recorded with old/new, User and Application", Allowed, !ok, format!("{got:?}"));
    }
    for (name, sql) in [
        ("ALTER TABLE ... DISABLE TRIGGER", "alter table app.orders disable trigger orders_history"),
        ("DROP TRIGGER", "drop trigger orders_history on app.orders"),
        ("SET session_replication_role = replica", "set session_replication_role = replica"),
        ("set_config('session_replication_role', 'replica')", "select set_config('session_replication_role', 'replica', true)"),
        ("INSERT a fake history row", "insert into vm.history (tbl, op, user_id, app_id) values ('app.orders', 'UPDATE', 'mallory', 'app1')"),
        ("UPDATE history rows", "update vm.history set user_id = 'mallory'"),
        ("DELETE history rows", "delete from vm.history"),
        ("TRUNCATE app.orders (row triggers would not fire)", "truncate app.orders"),
        ("TRUNCATE vm.history", "truncate vm.history"),
        ("ALTER TABLE app.orders ADD COLUMN", "alter table app.orders add column z int"),
        ("DROP TABLE app.orders", "drop table app.orders"),
        ("CREATE TABLE in app", "create table app.x (a int)"),
        ("CREATE FUNCTION in app", "create function app.f() returns int language sql as 'select 1'"),
        ("call the trigger function directly", "select vm.capture()"),
        ("replace vm.identity", "create or replace function vm.identity(out user_id text, out app_id text) language sql as $$ select 'x','y' $$"),
        ("DO block", "do $$ begin perform 1; end $$"),
    ] {
        let r = run_op(&env.rt, &c, &alice(), Level::Write, sql, &[]).await;
        env.rec(sec, name, Blocked, r.is_err(), res_str(&r));
    }
    // DO is not blocked by privileges (plpgsql USAGE is PUBLIC); show it runs.
    if let Some(last) = env.recs.last_mut() {
        if last.name == "DO block" {
            last.name = "DO block (PL/pgSQL USAGE is PUBLIC; harmless here because every write still fires the trigger)".into();
            last.expect = Info;
        }
    }
    // Anti-pattern: the runtime role owns the table.
    let r = run_op(&env.rt, &c, &alice(), Level::Write, "alter table app.rt_owned disable trigger rt_owned_history", &[]).await;
    env.rec(sec, "anti-pattern: table owned by app1_rt; App disables its trigger", Info, r.is_err(), res_str(&r));
}

// ----------------------------------------------------------- 4. transactions

async fn s4_transactions(env: &mut Env) {
    use Expect::*;
    let sec = "4 transactions";
    let c = signed(env, true);
    {
        let tx = begin(&env.rt, &c, &alice(), Level::Write).await.unwrap();
        let a = tx.exec("insert into app.orders (item, qty) values ('tx-1', 1) returning id").await;
        let b = tx.exec("update app.orders set qty = qty + 1 where item = 'tx-1'").await;
        let cm = tx.commit().await;
        let rows = env.su_app.query("select user_id, txid::text from vm.history where coalesce(new_row,old_row)->>'item' = 'tx-1'", &[]).await.unwrap();
        let ids: std::collections::BTreeSet<String> = rows.iter().map(|r| r.get::<_, String>(1)).collect();
        let users: std::collections::BTreeSet<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
        let ok = a.is_ok() && b.is_ok() && cm.is_ok() && rows.len() == 2 && ids.len() == 1 && users.len() == 1 && users.contains("alice@corp");
        env.rec(sec, "interactive: 2 statements in one transaction, identity attached once, both recorded as alice in one txid", Allowed, !ok, format!("{} history rows, users {users:?}, txids {ids:?}", rows.len()));
    }
    {
        let tx = begin(&env.rt, &c, &alice(), Level::Write).await.unwrap();
        let _ = tx.exec("insert into app.orders (item, qty) values ('abandoned', 1)").await;
        drop(tx); // the request ended without commit
        tokio::time::sleep(Duration::from_millis(200)).await;
        let n: i64 = env.su_app.query_one("select count(*) from app.orders where item = 'abandoned'", &[]).await.unwrap().get(0);
        env.rec(sec, "interactive: request ends without commit -> rolled back", Blocked, n == 0, format!("rows committed: {n}; idle connections back in pool: {}", env.rt.idle_count().await));
    }
    {
        let mut c2 = c.clone();
        c2.limits.tx_timeout = Duration::from_millis(500);
        let tx = begin(&env.rt, &c2, &alice(), Level::Write).await.unwrap();
        let _ = tx.exec("insert into app.orders (item, qty) values ('too-slow', 1)").await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let r = tx.exec("insert into app.orders (item, qty) values ('too-slow', 2)").await;
        let cm = tx.commit().await;
        let n: i64 = env.su_app.query_one("select count(*) from app.orders where item = 'too-slow'", &[]).await.unwrap().get(0);
        env.rec(sec, "interactive: transaction duration cap (500 ms) -> watchdog rolls back, later statements refused", Blocked, n == 0 && r.is_err() && cm.is_err(), format!("next stmt: {}; commit: {:?}; rows: {n}", res_str(&r), cm.err().map(|e| e.to_string())));
    }
    {
        let mut c2 = c.clone();
        c2.limits.tx_timeout = Duration::from_millis(800);
        let tx = begin(&env.rt, &c2, &alice(), Level::Write).await.unwrap();
        let t0 = Instant::now();
        let r = tx.exec("select pg_sleep(5)").await;
        env.rec(sec, "interactive: statement inside the tx is cut at the remaining tx time (800 ms)", Blocked, r.is_err() && t0.elapsed() < Duration::from_millis(2000), format!("{} after {:?}", res_str(&r), t0.elapsed()));
        drop(tx);
    }
    // App COMMITs in the middle of a host transaction (parser off).
    {
        let np = signed(env, false);
        let tx = begin(&env.rt, &np, &alice(), Level::Write).await.unwrap();
        let a = tx.exec("commit").await;
        let b = tx.exec("insert into app.orders (item, qty) values ('after-app-commit', 1)").await;
        drop(tx);
        env.rec(sec, "app DB, parser off: App sends COMMIT, then writes outside the host's transaction", Blocked, b.is_err(), format!("commit: {}; write: {}", res_str(&a), res_str(&b)));
    }
    for (name, parser) in [("parser off", false), ("parser on", true)] {
        let rc = cfg(parser, true, true, Identity::Label);
        let s0 = env.salary_sum().await;
        let tx = begin(&env.read, &rc, &alice(), Level::Read).await.unwrap();
        let a = tx.exec("commit").await;
        let b = tx.exec("select hr.give_raise()").await;
        drop(tx);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let changed = env.salary_sum().await != s0;
        env.reset_salary().await;
        env.rec(
            sec,
            format!("shared DB read level, {name}: App sends COMMIT (leaves READ ONLY), then calls a SECURITY DEFINER writer"),
            if parser { Blocked } else { Info },
            !changed,
            format!("commit: {}; write: {}", res_str(&a), res_str(&b)),
        );
    }
    {
        let rc = cfg(false, true, true, Identity::Label);
        let s0 = env.salary_sum().await;
        let tx = begin(&env.read, &rc, &alice(), Level::Read).await.unwrap();
        let a = tx.exec("set transaction read write").await;
        let b = tx.exec("select hr.give_raise()").await;
        drop(tx);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let changed = env.salary_sum().await != s0;
        env.reset_salary().await;
        env.rec(sec, "read level, parser off: SET TRANSACTION READ WRITE (host already ran a query after BEGIN)", Blocked, !changed, format!("{} / {}", res_str(&a), res_str(&b)));
    }
    {
        let r = run_batch(&env.rt, &c, &alice(), Level::Write, &[
            "insert into app.orders (item, qty) values ('batch', 1)",
            "insert into app.orders (item, qty) values ('batch', 2)",
            "update app.orders set qty = qty * 10 where item = 'batch'",
        ])
        .await;
        let n: i64 = env.su_app.query_one("select count(*) from vm.history where coalesce(new_row,old_row)->>'item' = 'batch' and user_id = 'alice@corp'", &[]).await.unwrap().get(0);
        env.rec(sec, "batch: 3 statements pipelined in one transaction, all recorded as alice", Allowed, !(r.is_ok() && n == 4), format!("ok={} history rows={n}", r.is_ok()));
        let r = run_batch(&env.rt, &c, &alice(), Level::Write, &[
            "insert into app.orders (item, qty) values ('batch-bad', 1)",
            "insert into app.orders (item, qty) values ('batch-bad', null)",
        ])
        .await;
        let n: i64 = env.su_app.query_one("select count(*) from app.orders where item = 'batch-bad'", &[]).await.unwrap().get(0);
        env.rec(sec, "batch: second statement fails -> first is rolled back", Blocked, r.is_err() && n == 0, format!("{:?} rows={n}", r.err().map(|e| e.to_string())));
    }
}

// ------------------------------------------------------------------ 5. limits

async fn s5_limits(env: &mut Env) {
    use Expect::*;
    let sec = "5 limits (shared DB, read login)";
    let full = cfg(true, true, true, Identity::Label);
    let nop = cfg(false, true, true, Identity::Label);
    {
        let t0 = Instant::now();
        let r = run_op(&env.read, &full, &alice(), Level::Read, "select pg_sleep(5)", &[]).await;
        env.rec(sec, "host statement timeout 1.5 s vs pg_sleep(5)", Blocked, r.is_err() && t0.elapsed() < Duration::from_millis(2500), format!("{} after {:?}", res_str(&r), t0.elapsed()));
    }
    {
        // Server-side GUC only: App lifts it.
        env.su_shared.batch_execute("alter role vm_read set statement_timeout = '500ms'").await.unwrap();
        // role settings apply at connection start: use a fresh pool
        let fresh = Pool::new(env.read.spec.clone());
        let mut long = nop.clone();
        long.limits.statement_timeout = Duration::from_secs(10);
        let tx = begin(&fresh, &long, &alice(), Level::Read).await.unwrap();
        let shown = tx.exec("select current_setting('statement_timeout')").await;
        let _ = tx.exec("select set_config('statement_timeout', '0', false)").await;
        let t0 = Instant::now();
        let r = tx.exec("select pg_sleep(1.2)").await;
        let el = t0.elapsed();
        drop(tx);
        env.su_shared.batch_execute("alter role vm_read reset statement_timeout").await.unwrap();
        env.rec(sec, "server-side statement_timeout (ALTER ROLE ... 500ms) lifted by App set_config, pg_sleep(1.2) completes", Info, r.is_err(), format!("role default seen: {}; sleep: {} after {el:?}", res_str(&shown), res_str(&r)));
    }
    {
        let tx = begin(&env.read, &nop, &alice(), Level::Read).await.unwrap();
        let _ = tx.exec("select set_config('statement_timeout', '0', true)").await;
        let t0 = Instant::now();
        let r = tx.exec("select pg_sleep(5)").await;
        drop(tx);
        env.rec(sec, "host timeout holds after App sets statement_timeout = 0 (parser off)", Blocked, r.is_err() && t0.elapsed() < Duration::from_millis(2500), format!("{} after {:?}", res_str(&r), t0.elapsed()));
    }
    {
        let t0 = Instant::now();
        let r = run_op(
            &env.read,
            &nop,
            &alice(),
            Level::Read,
            "do $$ begin loop begin perform pg_sleep(0.05); exception when query_canceled then null; end; end loop; end $$",
            &[],
        )
        .await;
        let el = t0.elapsed();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let n: i64 = env.su_shared.query_one("select count(*) from pg_stat_activity where usename = 'vm_read' and query like 'do $$ begin loop%'", &[]).await.unwrap().get(0);
        env.rec(sec, "PL/pgSQL swallows the cancel (parser off): host terminates the backend after the grace period", Blocked, r.is_err() && n == 0 && el < Duration::from_millis(3000), format!("{} after {el:?}; looping backends left: {n}", res_str(&r)));
    }
    {
        let t0 = Instant::now();
        let r = run_op(&env.read, &full, &alice(), Level::Read, "select generate_series(1, 50000000)", &[]).await;
        env.rec(sec, "row cap 1000 vs 50M-row result", Blocked, matches!(r, Err(HostError::RowCap(_))), format!("{} after {:?}", res_str(&r), t0.elapsed()));
        let t0 = Instant::now();
        let r = run_op(&env.read, &full, &alice(), Level::Read, "select repeat('x', 100000) from generate_series(1, 900)", &[]).await;
        env.rec(sec, "byte cap 4 MiB vs 900 x 100 kB rows", Blocked, matches!(r, Err(HostError::ByteCap(_))), format!("{} after {:?}", res_str(&r), t0.elapsed()));
        let t0 = Instant::now();
        let r = run_op(&env.read, &full, &alice(), Level::Read, "select repeat('x', 200000000)", &[]).await;
        env.rec(sec, "one 200 MB value: refused, but only after the whole row reached the host", Info, r.is_err(), format!("{} after {:?}", res_str(&r), t0.elapsed()));
    }
    {
        let r = run_op(&env.read, &nop, &alice(), Level::Read, "select set_config('transaction_timeout', '0', true)", &[]).await;
        env.rec(sec, "server-side transaction_timeout (PG 17+) can be set by App SQL", Info, r.is_err(), res_str(&r));
        let r = run_op(&env.read, &full, &alice(), Level::Read, "set statement_timeout = 0", &[]).await;
        env.rec(sec, "parser refuses SET statement_timeout (statement form)", Blocked, r.is_err(), res_str(&r));
    }
}

async fn s5_session_leaks(env: &mut Env) {
    use Expect::*;
    let sec = "5 session state left on a pooled connection for the next User";
    for discard in [false, true] {
        let tag = if discard { "with DISCARD ALL" } else { "no DISCARD" };
        let exp = if discard { Blocked } else { Info };
        let w = cfg(false, false, discard, Identity::Label);
        // fresh single-connection pool so the next op reuses the same backend
        let pool = Pool::new(env.write.spec.clone());
        let _ = run_op(&pool, &w, &alice(), Level::Write, "select set_config('search_path', 'it_private, hr', false)", &[]).await;
        let r = run_op(&pool, &w, &bob(), Level::Write, "select current_setting('search_path')", &[]).await;
        let leaked = r.as_ref().ok().and_then(|x| x.first()).map(|x| x[0].text().contains("it_private")).unwrap_or(false);
        env.rec(sec, format!("session search_path set by alice's op seen by bob's op [{tag}]"), exp, !leaked, res_str(&r));

        let _ = run_op(&pool, &w, &alice(), Level::Write, "create temp table leak as select 'alice-data' as v", &[]).await;
        let r = run_op(&pool, &w, &bob(), Level::Write, "select v from pg_temp.leak", &[]).await;
        env.rec(sec, format!("temp table created in alice's op read by bob's op [{tag}]"), exp, r.is_err(), res_str(&r));

        let _ = run_op(&pool, &w, &alice(), Level::Write, "select pg_advisory_lock(4242)", &[]).await;
        let held: bool = !env.su_shared.query_one("select pg_try_advisory_lock(4242)", &[]).await.unwrap().get::<_, bool>(0);
        let _ = env.su_shared.batch_execute("select pg_advisory_unlock_all()").await;
        env.rec(sec, format!("session advisory lock outlives alice's op [{tag}]"), exp, !held, format!("still held: {held}"));

        let _ = run_op(&pool, &w, &alice(), Level::Write, "select set_config('statement_timeout', '1', false)", &[]).await;
        let r = run_op(&pool, &w, &bob(), Level::Write, "select pg_sleep(0.05)", &[]).await;
        env.rec(sec, format!("statement_timeout = 1ms set by alice breaks bob's op [{tag}]"), exp, r.is_ok(), res_str(&r));
    }
}

// ------------------------------------------------------------- 6. structure

const STRUCTURE_SQL: &str = "
select n.nspname, c.relname, c.relkind::text, a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull,
       coalesce((select string_agg(con.contype::text, '') from pg_constraint con
                  where con.conrelid = c.oid and a.attnum = any(con.conkey)), '') as constraints
from pg_class c
join pg_namespace n on n.oid = c.relnamespace
join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped
where n.nspname = $1 and c.relkind in ('r','v','p')
order by c.relname, a.attnum";

async fn s6_structure(env: &mut Env) {
    use Expect::*;
    let sec = "6 structure level (vm_struct login: USAGE on hr, no SELECT)";
    let c = env.structp.get().await;
    let r = c.client.query(STRUCTURE_SQL, &[&"hr"]).await;
    let cols: Vec<String> = r.as_ref().map(|rows| rows.iter().map(|x| format!("{}.{} {}", x.get::<_, String>(1), x.get::<_, String>(3), x.get::<_, String>(4))).collect()).unwrap_or_default();
    env.rec(sec, "fixed pg_catalog query returns tables, columns, types, constraints", Allowed, cols.is_empty(), short(&format!("{} columns: {cols:?}", cols.len())));
    let r = c.client.simple_query("select * from hr.emp").await;
    env.rec(sec, "SELECT rows from hr.emp", Blocked, r.is_err(), format!("{:?}", r.map(|_| "ok").map_err(|e| pg_err(e).to_string())));
    let n: i64 = c.client.query_one("select count(*) from information_schema.columns where table_schema = 'hr'", &[]).await.unwrap().get(0);
    env.rec(sec, "information_schema.columns hides columns without privileges (so it cannot be the source)", Info, n == 0, format!("{n} columns visible"));
    let n: i64 = c.client.query_one("select count(*) from pg_stats where schemaname = 'hr'", &[]).await.unwrap().get(0);
    env.rec(sec, "pg_stats (most common values = data) for hr", Blocked, n == 0, format!("{n} rows visible to vm_struct"));
    let rd = env.read.get().await;
    let mcv: Option<String> = rd.client.query_one("select most_common_vals::text from pg_stats where schemaname = 'hr' and tablename = 'emp' and attname = 'status'", &[]).await.ok().and_then(|r| r.get(0));
    env.rec(sec, "same pg_stats query through the read login shows data values", Info, mcv.is_none(), format!("{mcv:?}"));
    env.read.put(rd).await;
    let v: String = c.client.query_one("select pg_get_viewdef('hr.ceo'::regclass)", &[]).await.unwrap().get(0);
    env.rec(sec, "view definition embeds a data literal ('Ms CEO')", Info, !v.contains("Ms CEO"), short(&v));
    let v: String = c
        .client
        .query_one(
            "select pg_get_constraintdef(oid) || ' / comment: ' || coalesce(col_description('hr.emp'::regclass, 3), '-') from pg_constraint where conrelid = 'hr.emp'::regclass and contype = 'c'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    env.rec(sec, "CHECK constraint text and comments are visible", Info, false, v);
    let v: f32 = c.client.query_one("select reltuples from pg_class where oid = 'hr.emp'::regclass", &[]).await.unwrap().get(0);
    env.rec(sec, "pg_class.reltuples (row-count estimate) is visible", Info, false, format!("{v}"));
    env.structp.put(c).await;
}

// -------------------------------------------------------------------- 8. cost

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    let p = |q: f64| v[((v.len() as f64 - 1.0) * q) as usize].as_micros();
    format!("median {:>5} us  p95 {:>5} us", p(0.5), p(0.95))
}

async fn bench(env: &mut Env) {
    const N: usize = 2000;
    println!("\n## 8 cost (localhost Docker, N={N} sequential ops per row after 200 warm-up)");
    let read_label = cfg(false, true, true, Identity::Label);
    let read_label_nodiscard = cfg(false, true, false, Identity::Label);
    let read_full = cfg(true, true, true, Identity::Label);
    let app_signed = cfg(true, false, true, Identity::Signed { secret: env.secret.clone() });
    let app_signed_nodiscard = cfg(true, false, false, Identity::Signed { secret: env.secret.clone() });
    let app_table = cfg(true, false, true, Identity::Table { secret: env.secret.clone() });
    let q = "select id, name from hr.emp where id = 2";

    async fn time<F, Fut>(mut f: F) -> Vec<Duration>
    where
        F: FnMut(usize) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        for i in 0..200 {
            f(i).await;
        }
        let mut v = Vec::with_capacity(N);
        for i in 0..N {
            let t = Instant::now();
            f(i).await;
            v.push(t.elapsed());
        }
        v
    }

    let bare = env.read.get().await;
    let st = bare.client.prepare(q).await.unwrap();
    let d = time(|_| async { bare.client.query(&st, &[]).await.unwrap(); }).await;
    println!("bare: prepared statement, pooled conn, autocommit           {}", stats(d));
    let d = time(|_| async { bare.client.query_typed(q, &[]).await.unwrap(); }).await;
    println!("bare: query_typed (1 round trip, unprepared), autocommit    {}", stats(d));
    env.read.put(bare).await;

    let pool = env.read.clone();
    let d = time(|_| async { run_op(&pool, &read_label_nodiscard, &alice(), Level::Read, q, &[]).await.unwrap(); }).await;
    println!("read op: BEGIN RO+label | query | COMMIT (no DISCARD)       {}", stats(d));
    let d = time(|_| async { run_op(&pool, &read_label, &alice(), Level::Read, q, &[]).await.unwrap(); }).await;
    println!("read op: + DISCARD ALL                                       {}", stats(d));
    let d = time(|_| async { run_op(&pool, &read_full, &alice(), Level::Read, q, &[]).await.unwrap(); }).await;
    println!("read op: + parser (recommended, shared DB)                   {}", stats(d));

    let rt = env.rt.clone();
    let bare = rt.get().await;
    let d = time(|i| {
        let s = format!("update app.plain set v = v + 1 where id = {}", i % 1000 + 1);
        let c = &bare.client;
        async move { c.query_typed(&s, &[]).await.unwrap(); }
    })
    .await;
    println!("bare: update app.plain (no trigger), autocommit             {}", stats(d));
    rt.put(bare).await;
    for (name, c, table) in [
        ("app op: signed identity, no trigger (app.plain)           ", &app_signed, "plain"),
        ("app op: signed identity, no DISCARD, no trigger           ", &app_signed_nodiscard, "plain"),
        ("app op: table identity (C), no trigger                    ", &app_table, "plain"),
        ("app op: signed identity + trigger, identity NOT verified   ", &app_signed, "bench_unv"),
        ("app op: signed identity + Change history trigger (recommended)", &app_signed, "bench"),
    ] {
        let d = time(|i| {
            let s = format!("update app.{table} set v = v + 1 where id = {}", i % 1000 + 1);
            let rt = rt.clone();
            let c = c.clone();
            async move { run_op(&rt, &c, &alice(), Level::Write, &s, &[]).await.unwrap(); }
        })
        .await;
        println!("{name} {}", stats(d));
    }
    // Bulk: per-row verification cost in the trigger.
    for table in ["plain", "bench_unv", "bench"] {
        let mut v = Vec::new();
        for _ in 0..20 {
            let t = Instant::now();
            run_op(&rt, &app_signed, &alice(), Level::Write, &format!("update app.{table} set v = v + 1"), &[]).await.unwrap();
            v.push(t.elapsed());
        }
        println!("app op: signed, update all 1000 rows of app.{table:<9} (n=20) {}", stats(v));
    }
    let mut v = Vec::new();
    for _ in 0..N {
        let t = Instant::now();
        policy::check("select e.id, e.name from hr.emp e join hr.dept d on d.head = e.id where e.salary > $1 order by e.name limit 50", Level::Read).unwrap();
        v.push(t.elapsed());
    }
    println!("parser: pg_query parse + checks of a 1-join SELECT, in-process {}", stats(v));
}
