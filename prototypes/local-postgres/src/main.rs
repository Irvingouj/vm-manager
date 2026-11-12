//! Spike: a private Postgres for the local development loop, obtained and run
//! by the program itself (no Docker, no admin rights, no manual install).
//!
//!   lp pg <root-dir>     one session: obtain (if needed), init (if needed),
//!                        start, run the checks, measure, stop.
//!   lp localhost         what resolves *.localhost on this machine.
//!
//! Prints `METRIC <name> <value>` and `PASS|FAIL <check>` lines; exits non-zero
//! if any check fails. Throwaway local passwords only.

use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use postgresql_embedded::{PostgreSQL, SettingsBuilder, VersionReq};
use tokio_postgres::{Client, NoTls};

/// Exact release; override with LP_PG_VERSION. (A third argument to `lp pg`
/// also sets it, for runs that do not inherit the environment, such as a
/// process started as another Windows user.)
fn pg_version() -> String {
    let v = std::env::args()
        .nth(3)
        .or_else(|| std::env::var("LP_PG_VERSION").ok())
        .unwrap_or_else(|| "18.6.0".into());
    format!("={v}")
}

/// Which binary distribution: "zonky" (default; Maven Central) or "theseus"
/// (GitHub releases). Fourth argument to `lp pg`, or LP_PG_SOURCE.
fn pg_source() -> String {
    std::env::args()
        .nth(4)
        .or_else(|| std::env::var("LP_PG_SOURCE").ok())
        .unwrap_or_else(|| "zonky".into())
}

/// Direct download URL (no GitHub API: it is rate-limited per IP), and the URL
/// of a checksum file if the distribution publishes one.
fn archive_url(source: &str, v: &str) -> Result<(String, Option<String>)> {
    match source {
        "theseus" => {
            let target = env!("LP_TARGET");
            let u = format!(
                "https://github.com/theseus-rs/postgresql-binaries/releases/download/{v}/postgresql-{v}-{target}.tar.gz"
            );
            Ok((u.clone(), Some(format!("{u}.sha256"))))
        }
        "zonky" => {
            let plat = match (std::env::consts::OS, std::env::consts::ARCH) {
                ("linux", "x86_64") => "linux-amd64",
                ("linux", "aarch64") => "linux-arm64v8",
                ("macos", "x86_64") => "darwin-amd64",
                ("macos", "aarch64") => "darwin-arm64v8",
                ("windows", "x86_64") => "windows-amd64",
                (o, a) => bail!("zonky has no build for {o}/{a}"),
            };
            let u = format!(
                "https://repo1.maven.org/maven2/io/zonky/test/postgres/embedded-postgres-binaries-{plat}/{v}/embedded-postgres-binaries-{plat}-{v}.jar"
            );
            Ok((u.clone(), Some(format!("{u}.sha1"))))
        }
        s => bail!("unknown source {s}"),
    }
}

async fn download(url: &str) -> Result<Vec<u8>> {
    let r = reqwest::get(url).await?.error_for_status()?;
    Ok(r.bytes().await?.to_vec())
}

fn first_hex_token(s: &str) -> String {
    s.split_whitespace().next().unwrap_or("").to_ascii_lowercase()
}

const CLUSTER_SQL: &str = include_str!("../sql/cluster.sql");
const SHARED_SQL: &str = include_str!("../sql/shared_db.sql");
const APP_SQL: &str = include_str!("../sql/app_db.sql");

fn metric(name: &str, value: impl std::fmt::Display) {
    println!("METRIC {name} {value}");
}

fn ms(d: Duration) -> String {
    format!("{}ms", d.as_millis())
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.filter_map(|e| e.ok())
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn mib(b: u64) -> String {
    format!("{:.1}MiB", b as f64 / 1048576.0)
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("pg") => {
            let arg = args.get(2).context("usage: lp pg <root-dir>")?;
            // "~/x": x under the per-user data directory (LOCALAPPDATA on
            // Windows, $HOME/.local/share elsewhere), as the product would use.
            let root = match arg.strip_prefix("~/") {
                Some(rest) if cfg!(windows) => PathBuf::from(std::env::var("LOCALAPPDATA")?).join(rest),
                Some(rest) => PathBuf::from(std::env::var("HOME")?).join(".local/share").join(rest),
                None => PathBuf::from(arg),
            };
            // The account the process really runs as (environment variables can
            // be inherited from another user, e.g. Windows Start-Process -Credential).
            let account = std::process::Command::new("whoami")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            println!("account: {account} os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
            pg_session(&root).await
        }
        Some("localhost") => localhost(),
        _ => bail!("usage: lp pg <root-dir> | lp localhost"),
    }
}

// --------------------------------------------------------------- *.localhost

fn localhost() -> Result<()> {
    let mut fails = 0;
    for name in ["localhost", "app.localhost", "my-app.localhost", "a.b.localhost", "APP.LOCALHOST"] {
        match (name, 80).to_socket_addrs() {
            Ok(a) => {
                let v: Vec<String> = a.map(|x| x.ip().to_string()).collect();
                let all_loop = !v.is_empty() && v.iter().all(|ip| ip == "127.0.0.1" || ip == "::1");
                println!("{} getaddrinfo({name}) -> {v:?}", if all_loop { "PASS" } else { "FAIL" });
                if !all_loop && name != "localhost" { fails += 1; }
            }
            Err(e) => {
                println!("FAIL getaddrinfo({name}) -> error: {e}");
                fails += 1;
            }
        }
    }
    // A tiny HTTP server on 127.0.0.1, reached by name through the OS resolver.
    let l = TcpListener::bind("127.0.0.1:0")?;
    let port = l.local_addr()?.port();
    std::thread::spawn(move || {
        // One thread per connection: browsers open speculative connections that
        // never send a request, which would block a sequential server.
        for s in l.incoming().flatten() {
          std::thread::spawn(move || {
            let mut s = s;
            // Read the whole request head before answering (closing with unread
            // input makes the kernel send RST).
            let mut req = String::new();
            let mut buf = [0u8; 1024];
            while !req.contains("\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
            let host = req.lines().find(|l| l.to_ascii_lowercase().starts_with("host:")).unwrap_or("").to_string();
            let body = format!("hello from {host}");
            let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
          });
        }
    });
    let name = "my-app.localhost";
    let r = (|| -> Result<String> {
        let mut s = TcpStream::connect((name, port))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.write_all(format!("GET / HTTP/1.1\r\nHost: {name}:{port}\r\nConnection: close\r\n\r\n").as_bytes())?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        Ok(out.lines().last().unwrap_or("").to_string())
    })();
    match r {
        Ok(body) => println!("PASS http via std TcpStream to {name}:{port} -> {body:?}"),
        Err(e) => { println!("FAIL http via std TcpStream to {name}:{port} -> {e}"); fails += 1; }
    }
    println!("LOCALHOST_PORT {port}");
    if std::env::var_os("LP_SERVE_SECS").is_some() {
        // Keep serving so an external client (curl, a browser) can try.
        let secs: u64 = std::env::var("LP_SERVE_SECS")?.parse()?;
        std::io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(secs));
    }
    if fails > 0 { bail!("{fails} localhost check(s) failed") }
    Ok(())
}

// ------------------------------------------------------------------ Postgres

async fn pg_session(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let version = pg_version().trim_start_matches('=').to_string();
    let source = pg_source();
    // LP_INSTALL_ROOT puts the binaries elsewhere than the data (to test paths).
    let install_root = std::env::var_os("LP_INSTALL_ROOT").map(PathBuf::from).unwrap_or_else(|| root.join("pg"));
    let install_dir = install_root.join(format!("{source}-{version}"));
    let data_dir = root.join("data");
    let pw_path = root.join("superuser-password");
    let secret_path = root.join("attach-secret");
    println!("root: {}", root.display());

    // 1. Obtain Postgres (first run only).
    if !install_dir.join("bin").exists() {
        let (url, sum_url) = archive_url(&source, &version)?;
        println!("downloading {url}");
        let t = Instant::now();
        let bytes = download(&url).await.with_context(|| format!("download {url}"))?;
        metric("download_time", ms(t.elapsed()));
        metric("download_size", mib(bytes.len() as u64));
        if let Some(sum_url) = sum_url {
            use sha1::Digest as _;
            let want = first_hex_token(&String::from_utf8_lossy(&download(&sum_url).await?));
            let got = if want.len() == 40 {
                hex::encode(sha1::Sha1::digest(&bytes))
            } else {
                hex::encode(sha2::Sha256::digest(&bytes))
            };
            if got != want {
                bail!("checksum mismatch for {url}: want {want}, got {got}");
            }
            println!("checksum ok ({} hex digits, from {sum_url})", want.len());
        }
        let key = if source == "zonky" {
            postgresql_archive::configuration::zonky::URL
        } else {
            postgresql_archive::configuration::theseus::URL
        };
        let t = Instant::now();
        let tmp = install_root.join("extracting");
        std::fs::create_dir_all(&install_root)?;
        let _ = std::fs::remove_dir_all(&tmp);
        postgresql_archive::extract(key, &bytes, &tmp).await?;
        std::fs::rename(&tmp, &install_dir)?;
        metric("extract_time", ms(t.elapsed()));
    } else {
        println!("binaries already present, no download");
    }
    metric("install_disk", mib(dir_size(&install_dir)));

    // Throwaway local superuser password, kept beside the cluster.
    let first = !data_dir.join("postgresql.conf").exists();
    if !pw_path.exists() {
        std::fs::write(&pw_path, hex::encode(rand::random::<[u8; 16]>()))?;
    }
    let password = std::fs::read_to_string(&pw_path)?;

    let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let settings = SettingsBuilder::new()
        .version(VersionReq::parse(&pg_version())?)
        .installation_dir(&install_dir)
        .trust_installation_dir(true)
        .data_dir(&data_dir)
        .password_file(&pw_path)
        .password(&password)
        .host("127.0.0.1")
        .port(port)
        .temporary(false)
        .timeout(Some(Duration::from_secs(180)))
        .config("listen_addresses", "127.0.0.1")
        .build();
    let mut pg = PostgreSQL::new(settings);

    // 2. Initialize the cluster (first run only).
    if first {
        let t = Instant::now();
        pg.setup().await.context("initdb")?;
        metric("init_time", ms(t.elapsed()));
    } else {
        pg.setup().await?; // no-op: installed and initialized
    }

    // 3. Start.
    let t = Instant::now();
    pg.start().await.context("pg_ctl start")?;
    let start = t.elapsed();
    metric(if first { "first_start_time" } else { "later_start_time" }, ms(start));
    metric("port", port);

    let outcome = run_all(port, &password, &secret_path, first).await;

    // Idle memory: let it settle, then sum the RSS of this cluster's processes.
    tokio::time::sleep(Duration::from_secs(3)).await;
    idle_memory(&data_dir, &install_dir);
    metric("data_disk", mib(dir_size(&data_dir)));

    // 4. Stop, whatever happened above.
    let t = Instant::now();
    let stopped = pg.stop().await;
    metric("stop_time", ms(t.elapsed()));
    stopped.context("pg_ctl stop")?;
    let still = data_dir.join("postmaster.pid").exists();
    println!("{} stopped cleanly (no postmaster.pid left)", if still { "FAIL" } else { "PASS" });

    let fails = outcome?;
    if fails > 0 || still {
        bail!("{} check(s) failed", fails + still as usize);
    }
    println!("ALL PASS");
    Ok(())
}

fn idle_memory(data_dir: &Path, install_dir: &Path) {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let postmaster: Option<u32> = std::fs::read_to_string(data_dir.join("postmaster.pid"))
        .ok()
        .and_then(|s| s.lines().next().and_then(|l| l.trim().parse().ok()));
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory().with_exe(UpdateKind::Always),
    );
    let canon = std::fs::canonicalize(install_dir).unwrap_or(install_dir.to_path_buf());
    let mut total = 0u64;
    let mut n = 0;
    let mut pm = 0u64;
    for (pid, p) in sys.processes() {
        let ours = p
            .exe()
            .map(|e| std::fs::canonicalize(e).unwrap_or(e.to_path_buf()).starts_with(&canon))
            .unwrap_or(false);
        if ours {
            total += p.memory();
            n += 1;
            if Some(pid.as_u32()) == postmaster {
                pm = p.memory();
            }
        }
    }
    metric("idle_processes", n);
    metric("idle_rss_sum", mib(total));
    metric("idle_rss_postmaster", mib(pm));
}

// -------------------------------------------------------------------- checks

async fn connect(port: u16, db: &str, user: &str, pw: &str) -> Result<Client, tokio_postgres::Error> {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .dbname(db)
        .user(user)
        .password(pw)
        .connect_timeout(Duration::from_secs(10))
        .connect(NoTls)
        .await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(client)
}

fn code(e: &tokio_postgres::Error) -> String {
    e.as_db_error()
        .map(|d| format!("{} {}", d.code().code(), d.message()))
        .unwrap_or_else(|| e.to_string())
}

struct Checks {
    fails: usize,
}

impl Checks {
    fn ok(&mut self, name: &str, pass: bool, detail: impl std::fmt::Display) {
        if pass {
            println!("PASS {name} [{detail}]");
        } else {
            println!("FAIL {name} [{detail}]");
            self.fails += 1;
        }
    }
    /// The statement must fail; `want` is the expected SQLSTATE.
    fn refused(&mut self, name: &str, r: Result<(), tokio_postgres::Error>, want: &str) {
        match r {
            Ok(()) => self.ok(name, false, "statement succeeded"),
            Err(e) => {
                let c = code(&e);
                self.ok(name, c.starts_with(want), c)
            }
        }
    }
}

async fn setup(port: u16, pw: &str, secret_path: &Path) -> Result<()> {
    let t = Instant::now();
    let su = connect(port, "postgres", "postgres", pw).await?;
    // CREATE DATABASE cannot run inside the implicit transaction of a multi-statement string.
    for stmt in CLUSTER_SQL
        .split(';')
        .filter(|x| x.lines().any(|l| !l.trim().is_empty() && !l.trim_start().starts_with("--")))
    {
        su.batch_execute(stmt).await.with_context(|| format!("cluster.sql: {stmt}"))?;
    }
    connect(port, "shared_db", "it_owner", "it").await?.batch_execute(SHARED_SQL).await.context("shared_db.sql")?;
    let mig = connect(port, "app1_db", "app1_migrator", "migrator").await?;
    mig.batch_execute(APP_SQL).await.context("app_db.sql")?;
    let secret = hex::encode(rand::random::<[u8; 32]>());
    let k1 = rand::random::<[u8; 32]>().to_vec();
    let k2 = rand::random::<[u8; 32]>().to_vec();
    mig.execute("insert into vm.keys values (sha256(convert_to($1::text,'UTF8')), $2, $3)", &[&secret, &k1, &k2])
        .await?;
    std::fs::write(secret_path, &secret)?;
    // Superuser-only fixture from the earlier spike: a table the runtime role owns.
    connect(port, "app1_db", "postgres", pw)
        .await?
        .batch_execute(
            "create table app.rt_owned (id int primary key, v int);
             create trigger rt_owned_history after insert or update or delete on app.rt_owned
               for each row execute function vm.capture();
             alter table app.rt_owned owner to app1_rt;",
        )
        .await?;
    metric("schema_setup_time", ms(t.elapsed()));
    Ok(())
}

async fn run_all(port: u16, pw: &str, secret_path: &Path, first: bool) -> Result<usize> {
    if first {
        setup(port, pw, secret_path).await?;
    }
    let secret = std::fs::read_to_string(secret_path)?;
    let mut c = Checks { fails: 0 };

    let su = connect(port, "postgres", "postgres", pw).await?;
    let v: String = su.query_one("select version()", &[]).await?.get(0);
    println!("server: {v}");
    let dd: String = su.query_one("show data_directory", &[]).await?.get(0);
    println!("data_directory: {dd}");
    let la: String = su.query_one("show listen_addresses", &[]).await?.get(0);
    c.ok("listens on loopback only", la == "127.0.0.1", format!("listen_addresses={la}"));
    let dbs: Vec<String> = su
        .query("select datname from pg_database where datname in ('shared_db','app1_db') order by 1", &[])
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    c.ok("two databases exist", dbs.len() == 2, format!("{dbs:?}"));

    let su_app = connect(port, "app1_db", "postgres", pw).await?;
    if !first {
        let n: i64 = su_app.query_one("select count(*) from vm.history", &[]).await?.get(0);
        c.ok("data from the previous run survived stop/start", n > 0, format!("{n} history rows"));
    }

    // --- Access levels on the shared database (ADR 0013): the role is the boundary.
    let read = connect(port, "shared_db", "vm_read", "read").await?;
    let n: i64 = read.query_one("select count(*) from hr.emp", &[]).await?.get(0);
    c.ok("read login can read hr.emp", n == 3, format!("{n} rows"));
    c.refused("read login cannot write hr.emp", read.batch_execute("insert into hr.emp values (99,'x',1)").await, "42501");
    c.refused("read login cannot read it_private", read.batch_execute("select * from it_private.secret").await, "42501");
    let write = connect(port, "shared_db", "vm_write", "write").await?;
    c.refused(
        "write login in a READ ONLY transaction cannot write",
        write.batch_execute("begin read only; update hr.emp set salary = salary + 1; commit").await,
        "25006",
    );
    let _ = write.batch_execute("rollback").await;
    let w = write.batch_execute("begin; update hr.emp set salary = salary + 1 where id = 1; rollback").await;
    c.ok("write login can write (control)", w.is_ok(), format!("{:?}", w.as_ref().map_err(code)));
    match connect(port, "app1_db", "vm_read", "read").await {
        Ok(_) => c.ok("shared-database login cannot connect to the Application's database", false, "connected"),
        Err(e) => c.ok("shared-database login cannot connect to the Application's database", code(&e).starts_with("42501"), code(&e)),
    }

    // --- Identity (SECURITY DEFINER functions) in the Application's own database.
    let rt = connect(port, "app1_db", "app1_rt", "rt").await?;
    let attach = format!("select vm.attach('alice@corp', 'app1', '{secret}')");
    let r = async {
        rt.batch_execute("begin").await?;
        rt.batch_execute(&attach).await?;
        let row = rt.query_one("select user_id from vm.identity()", &[]).await?;
        rt.batch_execute("insert into app.orders (item, qty) values ('widget', 2)").await?;
        rt.batch_execute("commit").await?;
        Ok::<String, tokio_postgres::Error>(row.get(0))
    }
    .await;
    c.ok("attached identity is read back by vm.identity() (SECURITY DEFINER)", r.as_deref().ok() == Some("alice@corp"), format!("{:?}", r.map_err(|e| code(&e))));
    let _ = rt.batch_execute("rollback").await;
    let n: i64 = su_app
        .query_one("select count(*) from vm.history where user_id = 'alice@corp' and new_row->>'item' = 'widget'", &[])
        .await?
        .get(0);
    c.ok("Change history trigger recorded alice's insert", n >= 1, format!("{n} rows"));

    c.refused(
        "forged identity (overwritten vm.ctx) is refused on write",
        rt.batch_execute(
            r#"begin; select set_config('vm.ctx', '{"u":"mallory@corp","a":"app1","t":"x","s":"00"}', true);
               insert into app.orders (item, qty) values ('forged', 1); commit"#,
        )
        .await,
        "42501",
    );
    let _ = rt.batch_execute("rollback").await;
    c.refused(
        "attach with a guessed secret is refused",
        rt.batch_execute("begin; select vm.attach('mallory@corp', 'app1', 'guess'); commit").await,
        "42501",
    );
    let _ = rt.batch_execute("rollback").await;
    let n: i64 = su_app.query_one("select count(*) from vm.history where user_id = 'mallory@corp'", &[]).await?.get(0);
    c.ok("no history row for the forged user", n == 0, format!("{n} rows"));

    // --- The runtime role cannot switch off the Change history.
    for (name, sql, want) in [
        ("runtime role cannot DISABLE TRIGGER", "alter table app.orders disable trigger orders_history", "42501"),
        ("runtime role cannot DROP TRIGGER", "drop trigger orders_history on app.orders", "42501"),
        ("runtime role cannot set session_replication_role", "set session_replication_role = replica", "42501"),
        ("runtime role cannot delete history", "delete from vm.history", "42501"),
    ] {
        let r = rt.batch_execute(&format!("begin; {attach}; {sql}; commit")).await;
        let _ = rt.batch_execute("rollback").await;
        c.refused(name, r, want);
    }
    let r = rt.batch_execute(&format!("begin; {attach}; alter table app.rt_owned disable trigger rt_owned_history; rollback")).await;
    let _ = rt.batch_execute("rollback").await;
    c.ok("anti-pattern control: a table the runtime role owns CAN have its trigger disabled", r.is_ok(), format!("{:?}", r.map_err(|e| code(&e))));

    // --- Several concurrent connections over the wire protocol.
    let t = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..20 {
        let attach = attach.clone();
        tasks.push(tokio::spawn(async move {
            let cl = connect(port, "app1_db", "app1_rt", "rt").await?;
            cl.batch_execute(&format!(
                "begin; {attach}; insert into app.orders (item, qty) values ('concurrent-{i}', 1); select pg_sleep(0.2); commit"
            ))
            .await?;
            Ok::<(), tokio_postgres::Error>(())
        }));
    }
    let mut ok = 0;
    for t in tasks {
        if matches!(t.await, Ok(Ok(()))) {
            ok += 1;
        }
    }
    c.ok("20 concurrent connections, each in its own transaction", ok == 20, format!("{ok}/20 ok in {}", ms(t.elapsed())));

    Ok(c.fails)
}
