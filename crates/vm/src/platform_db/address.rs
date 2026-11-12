//! `VM_DATABASE_URL`: where the platform database is, and as whom to log
//! in. It holds a password, so it is a type that does not print it: its
//! `Debug` hides the password, and no error made while reading it repeats
//! what was given.

use std::fmt;
use std::time::Duration;

/// The environment variable that names the platform database.
pub const VARIABLE: &str = "VM_DATABASE_URL";

/// The form `VM_DATABASE_URL` takes, for messages.
pub const FORM: &str = "postgres://<role>:<password>@<host>:<port>/<platform database>";

/// A password. It has no `Display`, and its `Debug` shows nothing of it.
pub struct Password(String);

impl Password {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<hidden>)")
    }
}

/// A Postgres connection address: `postgres://<role>[:<password>]@<host>[:<port>]/<database>`.
pub struct DatabaseAddress {
    host: String,
    port: u16,
    role: String,
    password: Option<Password>,
    database: String,
}

impl fmt::Debug for DatabaseAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseAddress")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("role", &self.role)
            .field("password", &self.password)
            .field("database", &self.database)
            .finish()
    }
}

impl DatabaseAddress {
    /// Reads an address. The error says what is wrong in words of its own
    /// and never repeats any part of `text`, which may hold the password.
    pub fn parse(text: &str) -> Result<Self, String> {
        let rest = text
            .strip_prefix("postgres://")
            .or_else(|| text.strip_prefix("postgresql://"))
            .ok_or("it does not start with postgres://")?;
        let (authority, path) = rest
            .split_once('/')
            .ok_or("it names no database: add /<platform database> after the host")?;
        if path.contains(['?', '#']) {
            return Err(
                "it has parameters after the database name, which the program does not read".into(),
            );
        }
        let database = decode(path, "the database name")?;
        if database.is_empty() {
            return Err("it names no database: add /<platform database> after the host".into());
        }
        // The password may hold an `@`, so the host is after the last one.
        let (userinfo, hostport) = authority
            .rsplit_once('@')
            .ok_or("it names no role: add <role>:<password>@ before the host")?;
        let (role, password) = match userinfo.split_once(':') {
            Some((role, password)) => (role, Some(password)),
            None => (userinfo, None),
        };
        let role = decode(role, "the role")?;
        if role.is_empty() {
            return Err("it names no role: add <role>:<password>@ before the host".into());
        }
        let password = password
            .map(|p| decode(p, "the password").map(Password))
            .transpose()?;
        let (host, port) = split_host_port(hostport)?;
        Ok(Self {
            host,
            port,
            role,
            password,
            database,
        })
    }

    /// The same Postgres and role, another database.
    pub fn with_database(&self, database: &str) -> Self {
        Self {
            host: self.host.clone(),
            port: self.port,
            role: self.role.clone(),
            password: self.password.as_ref().map(|p| Password(p.0.clone())),
            database: database.to_owned(),
        }
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    /// `host:port`, with brackets around an IPv6 address. Holds no secret.
    pub fn server(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The client's configuration. It holds the password; it is never
    /// printed.
    pub(crate) fn config(&self, connect_timeout: Duration) -> tokio_postgres::Config {
        let mut config = tokio_postgres::Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .user(&self.role)
            .dbname(&self.database)
            .application_name("vm")
            .connect_timeout(connect_timeout);
        if let Some(password) = &self.password {
            config.password(password.expose());
        }
        config
    }
}

fn split_host_port(hostport: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(bracketed) = hostport.strip_prefix('[') {
        let (host, after) = bracketed
            .split_once(']')
            .ok_or("its IPv6 host has no closing ]")?;
        let port = match after {
            "" => None,
            _ => Some(
                after
                    .strip_prefix(':')
                    .ok_or("something other than :<port> follows its IPv6 host")?,
            ),
        };
        (host, port)
    } else {
        match hostport.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err("it names no host".into());
    }
    if host.contains(['/', '@', '[', ']', ' ']) {
        return Err("its host is not a host name or address".into());
    }
    let port = match port {
        None => 5432,
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0 && port.bytes().all(|b| b.is_ascii_digit()))
            .ok_or("its port is not a number from 1 to 65535")?,
    };
    Ok((host.to_owned(), port))
}

/// Percent-decodes one part of the address into UTF-8.
fn decode(part: &str, what: &str) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(part.len());
    let mut rest = part.bytes();
    while let Some(byte) = rest.next() {
        if byte == b'%' {
            let high = rest.next().and_then(hex_value);
            let low = rest.next().and_then(hex_value);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(format!(
                    "{what} holds a % that is not followed by two hex digits"
                ));
            };
            bytes.push(high * 16 + low);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).map_err(|_| format!("{what} is not UTF-8 once decoded"))
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "s3cr%40t-Pw";

    #[test]
    fn a_full_address_is_read_and_decoded() {
        let address = DatabaseAddress::parse(&format!(
            "postgres://vm%5Fadmin:{SECRET}@db.example:6543/Vm-Platform"
        ))
        .unwrap();
        assert_eq!(address.role(), "vm_admin");
        assert_eq!(address.database(), "Vm-Platform");
        assert_eq!(address.server(), "db.example:6543");
        assert_eq!(address.password.as_ref().unwrap().expose(), "s3cr@t-Pw");
    }

    #[test]
    fn the_port_defaults_and_ipv6_keeps_its_brackets_in_messages() {
        let address = DatabaseAddress::parse("postgresql://r:p@[::1]/db").unwrap();
        assert_eq!(address.server(), "[::1]:5432");
        let address = DatabaseAddress::parse("postgres://r@[::1]:7/db").unwrap();
        assert_eq!(address.server(), "[::1]:7");
        assert!(address.password.is_none());
    }

    #[test]
    fn a_password_with_an_at_sign_is_kept_whole() {
        let address = DatabaseAddress::parse("postgres://r:a@b@127.0.0.1/db").unwrap();
        assert_eq!(address.password.as_ref().unwrap().expose(), "a@b");
        assert_eq!(address.server(), "127.0.0.1:5432");
    }

    #[test]
    fn what_cannot_be_read_is_refused_without_repeating_it() {
        for text in [
            String::new(),
            format!("this is not a url {SECRET}"),
            format!("postgres://vm:{SECRET}@127.0.0.1:not-a-port/db"),
            format!("postgres://vm:{SECRET}@127.0.0.1:99999999/db"),
            format!("postgres://vm:{SECRET}@127.0.0.1:0/db"),
            format!("postgres://vm:{SECRET}@[::1/db"),
            format!("postgres://vm:{SECRET}@127.0.0.1"),
            format!("postgres://vm:{SECRET}@127.0.0.1/"),
            format!("postgres://:{SECRET}@127.0.0.1/db"),
            format!("postgres://vm:{SECRET}@/db"),
            format!("postgres://vm:{SECRET}%zz@127.0.0.1/db"),
            format!("postgres://vm:{SECRET}@127.0.0.1/db?sslmode=require"),
            format!("mysql://vm:{SECRET}@127.0.0.1/db"),
        ] {
            let error = DatabaseAddress::parse(&text).expect_err(&text);
            assert!(
                !error.contains(SECRET) && !error.contains("s3cr"),
                "{error}"
            );
            assert!(!error.is_empty());
        }
    }

    #[test]
    fn debug_hides_the_password() {
        let address = DatabaseAddress::parse(&format!("postgres://vm:{SECRET}@h/db")).unwrap();
        let shown = format!("{address:?} {:?}", address.with_database("other"));
        assert!(!shown.contains("s3cr"), "{shown}");
        assert!(shown.contains("<hidden>"), "{shown}");
    }
}
