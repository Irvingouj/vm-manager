//! The smallest host that runs Application SQL the way the recommended design
//! says, with switches so the checks can turn each layer off and show what it
//! was holding.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{TryStreamExt, pin_mut};
use tokio::sync::Mutex;
use tokio_postgres::types::{FromSql, ToSql, Type};
use tokio_postgres::{Client, NoTls};

use crate::policy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Read,
    Write,
}

/// A value decoded from any column type, enough for the checks.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Bool(bool),
    Text(String),
    Other(usize),
}

impl Val {
    pub fn text(&self) -> String {
        match self {
            Val::Null => "NULL".into(),
            Val::Int(i) => i.to_string(),
            Val::Bool(b) => b.to_string(),
            Val::Text(s) => s.clone(),
            Val::Other(n) => format!("<{n} bytes>"),
        }
    }
}

pub struct Sized(pub Val, pub usize);

impl<'a> FromSql<'a> for Sized {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        let v = match *ty {
            Type::INT2 => Val::Int(i16::from_sql(ty, raw)? as i64),
            Type::INT4 => Val::Int(i32::from_sql(ty, raw)? as i64),
            Type::INT8 => Val::Int(i64::from_sql(ty, raw)?),
            Type::BOOL => Val::Bool(bool::from_sql(ty, raw)?),
            Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR | Type::UNKNOWN | Type::JSON => {
                Val::Text(String::from_utf8_lossy(raw).into_owned())
            }
            Type::JSONB => Val::Text(String::from_utf8_lossy(&raw[1..]).into_owned()),
            _ => Val::Other(raw.len()),
        };
        Ok(Sized(v, raw.len()))
    }
    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Sized(Val::Null, 0))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}

pub type Rows = Vec<Vec<Val>>;

#[derive(Debug)]
pub enum HostError {
    Policy(String),
    Pg(String),
    RowCap(usize),
    ByteCap(usize),
    StatementTimeout { terminated: bool },
    TxExpired,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Policy(s) => write!(f, "refused by host policy: {s}"),
            HostError::Pg(s) => write!(f, "postgres: {s}"),
            HostError::RowCap(n) => write!(f, "row cap {n} exceeded"),
            HostError::ByteCap(n) => write!(f, "byte cap {n} exceeded"),
            HostError::StatementTimeout { terminated } => {
                write!(f, "statement timeout (backend terminated: {terminated})")
            }
            HostError::TxExpired => write!(f, "transaction duration cap exceeded"),
        }
    }
}

pub fn pg_err(e: tokio_postgres::Error) -> HostError {
    match e.as_db_error() {
        Some(d) => HostError::Pg(format!("{} {}", d.code().code(), d.message())),
        None => HostError::Pg(e.to_string()),
    }
}

// ---------------------------------------------------------------- connections

pub struct Conn {
    pub client: Client,
    pub pid: i32,
}

#[derive(Clone)]
pub struct ConnSpec {
    pub port: u16,
    pub db: String,
    pub user: String,
    pub pw: String,
}

impl ConnSpec {
    pub fn new(port: u16, db: &str, user: &str, pw: &str) -> Self {
        Self { port, db: db.into(), user: user.into(), pw: pw.into() }
    }
    pub async fn connect(&self) -> anyhow::Result<Conn> {
        let cfg = format!(
            "host=127.0.0.1 port={} dbname={} user={} password={} application_name=vm-host",
            self.port, self.db, self.user, self.pw
        );
        let (client, conn) = tokio_postgres::connect(&cfg, NoTls).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let pid: i32 = client.query_one("select pg_backend_pid()", &[]).await?.get(0);
        Ok(Conn { client, pid })
    }
}

/// One pool per (database, login role). Idle connections are reused LIFO.
pub struct Pool {
    pub spec: ConnSpec,
    idle: Mutex<Vec<Conn>>,
}

impl Pool {
    pub fn new(spec: ConnSpec) -> Arc<Self> {
        Arc::new(Self { spec, idle: Mutex::new(Vec::new()) })
    }
    pub async fn get(&self) -> Conn {
        if let Some(c) = self.idle.lock().await.pop() {
            if !c.client.is_closed() {
                return c;
            }
        }
        self.spec.connect().await.expect("connect")
    }
    pub async fn put(&self, c: Conn) {
        if !c.client.is_closed() {
            self.idle.lock().await.push(c);
        }
    }
    pub async fn idle_count(&self) -> usize {
        self.idle.lock().await.len()
    }
}

// ---------------------------------------------------------------- host config

#[derive(Clone)]
pub enum Identity {
    /// Nothing installed in the database: host writes plain custom settings
    /// (a label only; Application SQL can overwrite it).
    Label,
    /// `vm.attach(user, app, secret)`: SECURITY DEFINER, sets a transaction-
    /// bound HMAC-signed `vm.ctx`. Recommended for Application-owned databases.
    Signed { secret: String },
    /// `vm.attach_c(user, app, secret)`: writes a row into a platform-owned
    /// unlogged table keyed by backend and transaction.
    Table { secret: String },
}

#[derive(Clone)]
pub struct Limits {
    pub statement_timeout: Duration,
    pub cancel_grace: Duration,
    pub max_rows: usize,
    pub max_bytes: usize,
    pub tx_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            statement_timeout: Duration::from_millis(1500),
            cancel_grace: Duration::from_millis(500),
            max_rows: 1000,
            max_bytes: 4 << 20,
            tx_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Clone)]
pub struct HostCfg {
    pub parser: bool,
    pub read_only_txn: bool,
    pub discard: bool,
    pub identity: Identity,
    pub limits: Limits,
}

#[derive(Clone)]
pub struct Ctx {
    pub user: String,
    pub app: String,
}

impl Ctx {
    pub fn new(user: &str, app: &str) -> Self {
        Self { user: user.into(), app: app.into() }
    }
}

fn attach_sql(id: &Identity) -> (&'static str, bool) {
    match id {
        Identity::Label => ("select set_config('vm.user', $1, true), set_config('vm.app', $2, true)", false),
        Identity::Signed { .. } => ("select vm.attach($1, $2, $3)::text", true),
        Identity::Table { .. } => ("select vm.attach_c($1, $2, $3)::text", true),
    }
}

async fn begin_and_attach(c: &Client, cfg: &HostCfg, ctx: &Ctx, level: Level) -> Result<(), HostError> {
    let begin = if level == Level::Read && cfg.read_only_txn { "BEGIN READ ONLY" } else { "BEGIN" };
    let (sql, needs_secret) = attach_sql(&cfg.identity);
    let secret = match &cfg.identity {
        Identity::Signed { secret } | Identity::Table { secret } => secret.clone(),
        Identity::Label => String::new(),
    };
    let mut p: Vec<(&(dyn ToSql + Sync), Type)> = vec![(&ctx.user, Type::TEXT), (&ctx.app, Type::TEXT)];
    if needs_secret {
        p.push((&secret, Type::TEXT));
    }
    // Pipelined: both messages go out before either answer is awaited.
    let (b, a) = tokio::join!(c.batch_execute(begin), c.query_typed(sql, &p));
    b.map_err(pg_err)?;
    a.map_err(pg_err)?;
    Ok(())
}

/// Run one statement with the host-side limits. Never lets Application SQL
/// choose its own limits: timeouts are enforced by cancel, then terminate.
async fn guarded_query(
    conn: &Conn,
    control: &ConnSpec,
    sql: &str,
    params: &[(String, Type)],
    limits: &Limits,
    timeout: Duration,
) -> Result<Rows, HostError> {
    let fut = async {
        let p: Vec<(&(dyn ToSql + Sync), Type)> =
            params.iter().map(|(v, t)| (v as &(dyn ToSql + Sync), t.clone())).collect();
        let stream = conn.client.query_typed_raw(sql, p).await.map_err(pg_err)?;
        pin_mut!(stream);
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = stream.try_next().await.map_err(pg_err)? {
            let mut r = Vec::with_capacity(row.len());
            for i in 0..row.len() {
                let Sized(v, n) = row.get::<_, Sized>(i);
                bytes += n;
                r.push(v);
            }
            rows.push(r);
            if rows.len() > limits.max_rows {
                return Err(HostError::RowCap(limits.max_rows));
            }
            if bytes > limits.max_bytes {
                return Err(HostError::ByteCap(limits.max_bytes));
            }
        }
        Ok(rows)
    };
    pin_mut!(fut);
    let res = tokio::select! {
        r = &mut fut => r,
        _ = tokio::time::sleep(timeout) => {
            let _ = conn.client.cancel_token().cancel_query(NoTls).await;
            tokio::select! {
                r = &mut fut => match r {
                    Err(HostError::Pg(s)) if s.starts_with("57014") => Err(HostError::StatementTimeout { terminated: false }),
                    other => other,
                },
                _ = tokio::time::sleep(limits.cancel_grace) => {
                    // Cancel was swallowed (e.g. a PL/pgSQL handler): terminate.
                    // A role may terminate its own role's backends, so the host
                    // needs no extra privilege for this.
                    if let Ok(ctl) = control.connect().await {
                        let _ = ctl.client.execute("select pg_terminate_backend($1)", &[&conn.pid]).await;
                    }
                    Err(HostError::StatementTimeout { terminated: true })
                }
            }
        }
    };
    // Row/byte cap hit: stop the server producing more.
    if matches!(res, Err(HostError::RowCap(_)) | Err(HostError::ByteCap(_))) {
        let _ = conn.client.cancel_token().cancel_query(NoTls).await;
    }
    res
}

async fn finish(pool: &Pool, conn: Conn, cfg: &HostCfg, ok: bool) {
    if conn.client.is_closed() {
        return;
    }
    let end = if ok { "COMMIT" } else { "ROLLBACK" };
    if cfg.discard {
        // Pipelined: COMMIT and DISCARD ALL are separate Query messages.
        let (a, b) = tokio::join!(conn.client.batch_execute(end), conn.client.batch_execute("DISCARD ALL"));
        if a.is_err() || b.is_err() {
            return; // drop the connection
        }
    } else if conn.client.batch_execute(end).await.is_err() {
        return;
    }
    pool.put(conn).await;
}

/// One operation = one transaction on a pooled connection of the role for
/// this access level.
pub async fn run_op(
    pool: &Pool,
    cfg: &HostCfg,
    ctx: &Ctx,
    level: Level,
    sql: &str,
    params: &[(String, Type)],
) -> Result<Rows, HostError> {
    if cfg.parser {
        policy::check(sql, level).map_err(HostError::Policy)?;
    }
    let conn = pool.get().await;
    let (a, q) = tokio::join!(
        begin_and_attach(&conn.client, cfg, ctx, level),
        guarded_query(&conn, &pool.spec, sql, params, &cfg.limits, cfg.limits.statement_timeout)
    );
    let res = a.and(q);
    finish(pool, conn, cfg, res.is_ok()).await;
    res
}

/// Several statements atomically in one operation: all pipelined in one round
/// trip, then COMMIT (or ROLLBACK).
pub async fn run_batch(
    pool: &Pool,
    cfg: &HostCfg,
    ctx: &Ctx,
    level: Level,
    stmts: &[&str],
) -> Result<Vec<Rows>, HostError> {
    if cfg.parser {
        for s in stmts {
            policy::check(s, level).map_err(HostError::Policy)?;
        }
    }
    let conn = pool.get().await;
    let qs = futures_util::future::join_all(
        stmts.iter().map(|s| guarded_query(&conn, &pool.spec, s, &[], &cfg.limits, cfg.limits.statement_timeout)),
    );
    let (a, qs) = tokio::join!(begin_and_attach(&conn.client, cfg, ctx, level), qs);
    let res = a.and_then(|_| qs.into_iter().collect::<Result<Vec<_>, _>>());
    finish(pool, conn, cfg, res.is_ok()).await;
    res
}

// ------------------------------------------------- interactive transactions

struct TxInner {
    conn: Option<Conn>,
    expired: bool,
}

/// A transaction held open across several host operations. The host holds the
/// connection; the Application holds only an opaque handle. A watchdog rolls
/// back at the duration cap; dropping the handle (request ended) rolls back.
pub struct Tx {
    inner: Arc<Mutex<TxInner>>,
    pool: Arc<Pool>,
    cfg: HostCfg,
    level: Level,
    deadline: Instant,
}

pub async fn begin(pool: &Arc<Pool>, cfg: &HostCfg, ctx: &Ctx, level: Level) -> Result<Tx, HostError> {
    let conn = pool.get().await;
    if let Err(e) = begin_and_attach(&conn.client, cfg, ctx, level).await {
        finish(pool, conn, cfg, false).await;
        return Err(e);
    }
    let inner = Arc::new(Mutex::new(TxInner { conn: Some(conn), expired: false }));
    let deadline = Instant::now() + cfg.limits.tx_timeout;
    {
        let inner = inner.clone();
        let pool = pool.clone();
        let cfg = cfg.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline.into()).await;
            let mut g = inner.lock().await;
            if let Some(c) = g.conn.take() {
                g.expired = true;
                finish(&pool, c, &cfg, false).await;
            }
        });
    }
    Ok(Tx { inner, pool: pool.clone(), cfg: cfg.clone(), level, deadline })
}

impl Tx {
    pub async fn exec(&self, sql: &str) -> Result<Rows, HostError> {
        if self.cfg.parser {
            policy::check(sql, self.level).map_err(HostError::Policy)?;
        }
        let g = self.inner.lock().await;
        let Some(conn) = g.conn.as_ref() else { return Err(HostError::TxExpired) };
        let left = self.deadline.saturating_duration_since(Instant::now());
        let t = left.min(self.cfg.limits.statement_timeout);
        guarded_query(conn, &self.pool.spec, sql, &[], &self.cfg.limits, t).await
    }
    pub async fn commit(self) -> Result<(), HostError> {
        let mut g = self.inner.lock().await;
        let Some(conn) = g.conn.take() else { return Err(HostError::TxExpired) };
        let r = conn.client.batch_execute("COMMIT").await.map_err(pg_err);
        if self.cfg.discard {
            let _ = conn.client.batch_execute("DISCARD ALL").await;
        }
        self.pool.put(conn).await;
        r
    }
}

impl Drop for Tx {
    fn drop(&mut self) {
        // Request ended without commit: roll back and return the connection.
        let inner = self.inner.clone();
        let pool = self.pool.clone();
        let cfg = self.cfg.clone();
        tokio::spawn(async move {
            let mut g = inner.lock().await;
            if let Some(c) = g.conn.take() {
                finish(&pool, c, &cfg, false).await;
            }
        });
    }
}
