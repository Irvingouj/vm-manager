# Audit has two layers: an operation log and a change history kept beside the data

Auditing answers two different questions, so it is kept in two places. The **Operation log** answers "who touched this Resource": for every operation, read or write, it records the User, the Application, the Resource, the statement text without parameter values, rows returned or affected, duration and outcome. It holds no data from the Resource, lives in the platform's own database, and is read by IT and auditors. The **Change history** answers "how did this value come to be": for every write it records the row, the columns changed, the old and new values, the User and the Application. It is the equivalent of a spreadsheet's revision history, and it is what makes a record such as "who approved this" trustworthy, because the identity in it is written by the platform and cannot be set by Application code.

The Change history contains real data, so it is stored in the same database as the data it describes and is readable under the same Grant. It is therefore exposed to nobody the data itself is not exposed to, and people who can read the Operation log gain no view of the data.

Only the database can record changes reliably, so the Change history is captured inside it, by triggers writing to a history table, and not by interpreting SQL in the host: a statement such as `set amount = amount * 1.1` contains neither the rows nor the values. Two things follow. Every operation must carry the User's and the Application's identity into the Postgres session in a way Application SQL cannot overwrite. A later spike showed this is achievable only where the platform can install its own functions in the database, which in Milestone 1 means an Application's own database (ADR 0013). And installing the capture is a change of structure, so only the Schema owner can decide it.

In Milestone 1 the Operation log covers every Resource, and the Change history is installed automatically on every Application's own database, where the platform already applies the structure. Change history on a shared database, switched on by its Schema owner, is roadmap.

## Considered Options

- **One audit log that includes parameter values**: turns the audit store into a copy of the sensitive data, readable by auditors who hold no Grant on it, and still cannot show old values.
- **Statement text only**: shows that someone updated the salary table, never what changed.
