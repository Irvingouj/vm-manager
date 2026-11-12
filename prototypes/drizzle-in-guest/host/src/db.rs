//! The host operations a guest's database calls become.
//!
//! `env.HOST.invoke(name, args)`:
//!
//! - `sql {db, sql, params, method, tx?}` runs one statement on the Resource
//!   `db` (or inside the open transaction `tx`) and answers `{rows}`.
//! - `tx.begin {db}` takes a connection for this request, runs BEGIN, answers
//!   `{tx}`. `tx.commit {tx}` / `tx.rollback {tx}` end it and give the
//!   connection back. A transaction belongs to the request that began it; the
//!   host rolls back whatever is still open when the request ends.
//!
//! Every statement is classified with `sqlparser` and logged (the capture);
//! with `strict`, anything but one SELECT/INSERT/UPDATE/DELETE is refused
//! before it reaches Postgres.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use tokio_postgres::{Client, NoTls};

use crate::pg;

/// One Resource: a database the guest names by its logical name.
pub struct Resource {
    conninfo: String,
    shared: Client,
    free: Mutex<Vec<Client>>,
}

async fn connect(conninfo: &str) -> Result<Client, String> {
    let (client, connection) = tokio_postgres::connect(conninfo, NoTls)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("postgres connection: {e}");
        }
    });
    Ok(client)
}

impl Resource {
    pub async fn new(conninfo: &str) -> Result<Self, String> {
        Ok(Self {
            conninfo: conninfo.to_string(),
            shared: connect(conninfo).await?,
            free: Mutex::new(Vec::new()),
        })
    }

    async fn take(&self) -> Result<Client, String> {
        let pooled = self.free.lock().unwrap().pop();
        match pooled {
            Some(c) if !c.is_closed() => Ok(c),
            _ => connect(&self.conninfo).await,
        }
    }

    fn give_back(&self, client: Client) {
        if !client.is_closed() {
            self.free.lock().unwrap().push(client);
        }
    }
}

pub type Resources = Arc<HashMap<String, Arc<Resource>>>;

struct Tx {
    request_id: u64,
    resource: Arc<Resource>,
    client: Arc<Client>,
}

static NEXT_TX: AtomicU64 = AtomicU64::new(1);

/// What the host knows about one statement.
pub fn classify(sql: &str) -> Value {
    match Parser::parse_sql(&PostgreSqlDialect {}, sql) {
        Ok(stmts) => {
            let kinds: Vec<&str> = stmts
                .iter()
                .map(|s| match s {
                    Statement::Query(_) => "select",
                    Statement::Insert(_) => "insert",
                    Statement::Update { .. } => "update",
                    Statement::Delete(_) => "delete",
                    _ => "other",
                })
                .collect();
            json!({ "parsed": true, "count": stmts.len(), "kinds": kinds })
        }
        Err(e) => json!({ "parsed": false, "error": e.to_string() }),
    }
}

fn allowed(class: &Value) -> Result<(), String> {
    if class["parsed"] != true {
        return Err(format!("refused: the host cannot parse the statement ({})", class["error"]));
    }
    if class["count"] != 1 {
        return Err(format!("refused: {} statements in one call", class["count"]));
    }
    match class["kinds"][0].as_str() {
        Some("select" | "insert" | "update" | "delete") => Ok(()),
        _ => Err("refused: only SELECT, INSERT, UPDATE and DELETE".into()),
    }
}

pub struct Db {
    resources: Resources,
    txs: Mutex<HashMap<u64, Tx>>,
    pub capture: Arc<Mutex<Vec<Value>>>,
    strict: bool,
}

impl Db {
    pub fn new(resources: Resources, capture: Arc<Mutex<Vec<Value>>>, strict: bool) -> Self {
        Self { resources, txs: Mutex::default(), capture, strict }
    }

    fn resource(&self, args: &Value) -> Result<Arc<Resource>, String> {
        let name = args["db"].as_str().ok_or("no db")?;
        self.resources.get(name).cloned().ok_or_else(|| format!("no Resource {name}"))
    }

    /// `sql`: one statement.
    pub async fn sql(&self, request_id: u64, args: Value) -> Result<Value, String> {
        let text = args["sql"].as_str().ok_or("no sql")?.to_string();
        let method = args["method"].as_str().unwrap_or("execute").to_string();
        let params = args["params"].as_array().cloned().unwrap_or_default();
        let class = classify(&text);
        let refused = if self.strict { allowed(&class).err() } else { None };
        self.capture.lock().unwrap().push(json!({
            "db": args["db"], "tx": args["tx"], "sql": text, "params": params,
            "method": method, "class": class, "refused": refused,
        }));
        if let Some(reason) = refused {
            return Err(reason);
        }

        let client: Arc<Client> = match args["tx"].as_u64() {
            Some(id) => {
                let txs = self.txs.lock().unwrap();
                let tx = txs.get(&id).ok_or_else(|| format!("no open transaction {id}"))?;
                if tx.request_id != request_id {
                    return Err(format!("transaction {id} belongs to another request"));
                }
                tx.client.clone()
            }
            None => {
                let r = self.resource(&args)?;
                return run(&r.shared, &text, &params, &method).await;
            }
        };
        run(&client, &text, &params, &method).await
    }

    pub async fn begin(&self, request_id: u64, args: Value) -> Result<Value, String> {
        let resource = self.resource(&args)?;
        // Drizzle's PgTransactionConfig, from fixed words only.
        let config = &args["config"];
        let mut begin = String::from("BEGIN");
        match config["isolationLevel"].as_str() {
            None => {}
            Some(level @ ("read uncommitted" | "read committed" | "repeatable read" | "serializable")) => {
                begin += &format!(" ISOLATION LEVEL {level}")
            }
            Some(other) => return Err(format!("bad isolation level {other}")),
        }
        match config["accessMode"].as_str() {
            None => {}
            Some(mode @ ("read only" | "read write")) => begin += &format!(" {mode}"),
            Some(other) => return Err(format!("bad access mode {other}")),
        }
        match config["deferrable"].as_bool() {
            None => {}
            Some(true) => begin += " DEFERRABLE",
            Some(false) => begin += " NOT DEFERRABLE",
        }
        let client = resource.take().await?;
        client.batch_execute(&begin).await.map_err(|e| e.to_string())?;
        let id = NEXT_TX.fetch_add(1, Ordering::Relaxed);
        self.capture.lock().unwrap().push(json!({ "db": args["db"], "tx": id, "op": "tx.begin", "begin": begin }));
        self.txs.lock().unwrap().insert(id, Tx { request_id, resource, client: Arc::new(client) });
        Ok(json!({ "tx": id }))
    }

    pub async fn end(&self, request_id: u64, args: Value, commit: bool) -> Result<Value, String> {
        let id = args["tx"].as_u64().ok_or("no tx")?;
        let tx = {
            let mut txs = self.txs.lock().unwrap();
            match txs.get(&id) {
                Some(tx) if tx.request_id == request_id => txs.remove(&id).unwrap(),
                Some(_) => return Err(format!("transaction {id} belongs to another request")),
                None => return Err(format!("no open transaction {id}")),
            }
        };
        let op = if commit { "tx.commit" } else { "tx.rollback" };
        self.capture.lock().unwrap().push(json!({ "tx": id, "op": op }));
        let result = tx.client.batch_execute(if commit { "COMMIT" } else { "ROLLBACK" }).await;
        if let Ok(client) = Arc::try_unwrap(tx.client) {
            tx.resource.give_back(client);
        }
        result.map(|_| json!({ "tx": id, "ended": op })).map_err(|e| e.to_string())
    }

    /// Rolls back every transaction the request left open; answers how many.
    pub async fn abandon(&self, request_id: u64) -> usize {
        let open: Vec<u64> = self
            .txs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, tx)| tx.request_id == request_id)
            .map(|(id, _)| *id)
            .collect();
        for id in &open {
            self.capture.lock().unwrap().push(json!({ "tx": id, "op": "tx.abandoned-by-request-end" }));
            let _ = self.end(request_id, json!({ "tx": id }), false).await;
        }
        open.len()
    }
}

async fn run(client: &Client, text: &str, params: &[Value], method: &str) -> Result<Value, String> {
    let stmt = client.prepare(text).await.map_err(|e| db_error(&e))?;
    let types = stmt.params();
    if types.len() != params.len() {
        return Err(format!("{} parameters given, the statement has {}", params.len(), types.len()));
    }
    let bound: Vec<pg::TextParam> = params.iter().zip(types).map(|(v, t)| pg::param(v, t)).collect();
    let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
        bound.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
    let rows = client.query(&stmt, &refs).await.map_err(|e| db_error(&e))?;
    let rows = pg::rows(&rows, method).map_err(|e| e.to_string())?;
    Ok(json!({ "rows": rows }))
}

fn db_error(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => format!("{}: {}", db.code().code(), db.message()),
        None => e.to_string(),
    }
}
