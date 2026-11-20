# The tool's commands

Every command of the tool `vibe` 0.1.1, as `vibe <command> --help` prints it. `vibe --help` lists them all; that list and the exit codes are at the end of this page.

## Signing in

### `vibe sign-in`

```text
Sign the tool in to an installation, and keep its credential in your own configuration folder

Usage: vibe sign-in [OPTIONS] --domain <DOMAIN>

Options:
      --domain <DOMAIN>
          The installation's domain: the tool speaks HTTPS to api.<domain>
      --port <PORT>
          The port of HTTPS (443 when not given)
      --resolve <RESOLVE>
          Connect to this IP address (IPv4 or IPv6, written alone) in place of what the system resolves api.<domain> to; nothing else changes: the certificate is checked for api.<domain>
      --connect <CONNECT>
          Plain HTTP instead, to an installation on this machine: a loopback address and port, such as 127.0.0.1:8080 or [::1]:8080
      --stand-in-user <STAND_IN_USER>
          Development only, while sign-in is the stand-in: the User to sign in as
      --stand-in-groups <STAND_IN_GROUPS>
          Development only: the User's Groups, comma-separated
      --no-browser
          Do not open a browser here: show an address to open on any device, and a check code to type there
      --timeout <TIMEOUT>
          How long to wait for the sign-in in the browser, in seconds [default: 300]
      --json
          Print exactly one JSON object on standard output
  -h, --help
          Print help
```

### `vibe sign-out`

```text
End the tool's credential at the installation and remove it from this machine

Usage: vibe sign-out [OPTIONS]

Options:
      --json  Print exactly one JSON object on standard output
  -h, --help  Print help
```

### `vibe whoami`

```text
Who the tool is signed in as: the User, their Groups, the installation

Usage: vibe whoami [OPTIONS]

Options:
      --json  Print exactly one JSON object on standard output
  -h, --help  Print help
```


## Making an Application

### `vibe new`

```text
Make a new project from the template: an Application that answers one page, ready for npm install and vibe check

Usage: vibe new [OPTIONS] <NAME>

Arguments:
  <NAME>  The Application's name: a lower-case DNS label (letters, digits and hyphens), which becomes its address

Options:
      --directory <DIRECTORY>  Where to make the project: a missing or empty folder. Without it, ./<name>
      --json                   Print exactly one JSON object on standard output
  -h, --help                   Print help
```

### `vibe check`

```text
Check whether the project will deploy: its Manifest, its Resources, its migrations and its types, every problem with its fix

Usage: vibe check [OPTIONS]

Options:
      --project <PROJECT>  The project to check; without it, the current directory
      --timeout <TIMEOUT>  The longest the whole check may take, in seconds (1 to 3600; 300 without it)
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```

### `vibe generate`

```text
Generate the next migration from src/db/schema.ts into migrations/, stopping before one that destroys data

Usage: vibe generate [OPTIONS]

Options:
      --project <PROJECT>  The project; without it, the current directory
      --name <NAME>        The migration's name after its number (0001_<name>): 1 to 100 lower-case letters, digits and _. Without it, drizzle-kit's own
      --allow-data-loss    Generate a migration that destroys data (drops a table, a column or a schema, or changes a type to one that is not wider). Only when the Builder has clearly said that this data may be lost
      --empty              Write an empty migration, for SQL of your own (such as copying values); only when the schema is in step with the migrations
      --timeout <TIMEOUT>  The longest the command may take, in seconds (1 to 3600; 300 without it)
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```


## Shared databases

### `vibe resources`

```text
Every registered Resource: its description, your Access level, and whom to ask

Usage: vibe resources [OPTIONS]

Options:
      --json  Print exactly one JSON object on standard output
  -h, --help  Print help
```

### `vibe pull`

```text
Pull a Resource's structure into the project, as a typed Drizzle definition (resources/<name>/schema.ts) and its link (resources/<name>/index.ts)

Usage: vibe pull [OPTIONS] <RESOURCE>

Arguments:
  <RESOURCE>  The Resource whose structure to pull

Options:
      --project <PROJECT>  The project to write into; without it, the current directory
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```


## Running it on your computer

### `vibe dev`

```text
Keep the project running on this machine and serve it to your browser at http://<name>.localhost:7480/, as the test User you choose there; rebuilt when the project changes, until interrupted

Usage: vibe dev [OPTIONS]

Options:
      --port <PORT>        The port to listen on, on 127.0.0.1 and ::1 only (1 to 65535) [default: 7480]
      --project <PROJECT>  The project; without it, the current directory
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```

### `vibe request`

```text
Run the project on this machine, beside a private Postgres with an empty stand-in of each pulled Resource, and send it one request as a test User of vibe.dev.json (--as builder, --as outsider, ...)

Usage: vibe request [OPTIONS] --as <TEST USER> <PATH>

Arguments:
  <PATH>  The path to ask for: it begins with /, and may have a query

Options:
      --as <TEST USER>         The test User of vibe.dev.json to send the request as (builder, outsider, ...)
      --method <METHOD>        The method: GET without a body, POST with one; any token is taken
      --header <HEADER>        A header, "<name>: <value>"; may be given several times. Host, Content-Length and x-vm-* are the tool's
      --data <DATA>            The body, as text (at most 1 MiB)
      --data-file <DATA_FILE>  The body, the bytes of this file (at most 1 MiB)
      --project <PROJECT>      The project; without it, the current directory
      --timeout <TIMEOUT>      The longest the whole run may take, in seconds (1 to 3600; 300 without it)
      --json                   Print exactly one JSON object on standard output
  -h, --help                   Print help
```

### `vibe local`

```text
The project's local runs: what they recorded (operation-log, change-history), and reset, which starts the local databases over

Usage: vibe local <COMMAND>

Commands:
  operation-log   The local Operation log: the newest entries of the project's local runs, oldest first
  change-history  The local Change history of the own database: the newest changes first, of one table or of all
  reset           Start the local databases over: the own database, the stand-ins, the local Versions and records are made again from nothing, with every migration and the test rows. The project is not touched
  help            Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `vibe local operation-log`

```text
The local Operation log: the newest entries of the project's local runs, oldest first

Usage: vibe local operation-log [OPTIONS]

Options:
      --limit <LIMIT>      How many entries, the newest (1 to 1000; 100 without it) [default: 100]
      --project <PROJECT>  The project; without it, the current directory
      --timeout <TIMEOUT>  The longest the whole command may take, in seconds (1 to 3600; 300 without it)
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```

### `vibe local change-history`

```text
The local Change history of the own database: the newest changes first, of one table or of all

Usage: vibe local change-history [OPTIONS]

Options:
      --table <TABLE>      Only the changes of this table: [schema.]table (public without a schema)
      --limit <LIMIT>      How many changes, the newest (1 to 1000; 100 without it) [default: 100]
      --project <PROJECT>  The project; without it, the current directory
      --timeout <TIMEOUT>  The longest the whole command may take, in seconds (1 to 3600; 300 without it)
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```

### `vibe local reset`

```text
Start the local databases over: the own database, the stand-ins, the local Versions and records are made again from nothing, with every migration and the test rows. The project is not touched

Usage: vibe local reset [OPTIONS]

Options:
      --project <PROJECT>  The project; without it, the current directory
      --timeout <TIMEOUT>  The longest the whole reset may take, in seconds (1 to 3600; 300 without it)
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```


## Deploying

### `vibe deploy`

```text
Deploy the project: build it on this machine from its last commit, check it, and send it as the Application's new Version, which goes live at its address

Usage: vibe deploy [OPTIONS]

Options:
      --project <PROJECT>  The project to deploy; without it, the current directory
      --allow-data-loss    Deploy although a migration this Version adds destroys data while the Application already has a Version. Only when the Builder has clearly said that this data may be lost
      --timeout <TIMEOUT>  The longest the deploy may take on this machine, in seconds (1 to 3600; 300 without it). Sending the Version has its own end
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```

### `vibe versions`

```text
The Application's Versions: each one's commit, when and by whom it was deployed, and which one serves

Usage: vibe versions [OPTIONS] <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --limit <LIMIT>  How many of the newest Versions to print: 1 to 1000 [default: 100]
      --json           Print exactly one JSON object on standard output
  -h, --help           Print help
```

### `vibe go-back`

```text
Serve an earlier Version again, at once: the one before the serving Version, or --to <number>. Migrations are not undone

Usage: vibe go-back [OPTIONS] <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --to <TO>  The Version to serve instead: an earlier one than the serving Version. Without it, the one just before
      --json     Print exactly one JSON object on standard output
  -h, --help     Print help
```

### `vibe logs`

```text
What the Application printed and the errors it did not catch, request by request; --follow prints new entries as they come

Usage: vibe logs [OPTIONS] <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --request <REQUEST>  Only the entry of this request: a response's x-vm-request-id
      --after <AFTER>      Only the entries after this cursor: the `next` of an earlier run
      --limit <LIMIT>      How many entries, 1 to 1000 (100 without it): the newest, or the oldest after --after
      --follow             Go on printing new entries as they are made, until interrupted
      --json               Print exactly one JSON object on standard output
  -h, --help               Print help
```


## Sharing and Contributors

### `vibe applications`

```text
The Applications you contribute to and those shared with you: name, description, address, and which is which

Usage: vibe applications [OPTIONS]

Options:
      --json  Print exactly one JSON object on standard output
  -h, --help  Print help
```

### `vibe show`

```text
An Application: its address, serving Version, Contributors and whom it is shared with

Usage: vibe show [OPTIONS] <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --json  Print exactly one JSON object on standard output
  -h, --help  Print help
```

### `vibe share`

```text
Share the Application: let people, Groups or everyone signed in open it (--user, --email, --group, --everyone; each as often as needed)

Usage: vibe share [OPTIONS] <--user <ID>|--email <ADDRESS>|--group <GROUP>|--everyone> <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --user <ID>        A person, by their user id (vibe whoami shows it)
      --email <ADDRESS>  A person, by their email address: the one who signed in here with it
      --group <GROUP>    A Group, by its name
      --everyone         Everyone who can sign in to the installation
      --json             Print exactly one JSON object on standard output
  -h, --help             Print help
```

### `vibe unshare`

```text
Stop sharing the Application with people, Groups or everyone

Usage: vibe unshare [OPTIONS] <--user <ID>|--email <ADDRESS>|--group <GROUP>|--everyone> <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --user <ID>        A person, by their user id (vibe whoami shows it)
      --email <ADDRESS>  A person, by their email address: the one who signed in here with it
      --group <GROUP>    A Group, by its name
      --everyone         Everyone who can sign in to the installation
      --json             Print exactly one JSON object on standard output
  -h, --help             Print help
```

### `vibe contributors`

```text
Change who maintains the Application: vibe contributors add|remove

Usage: vibe contributors <COMMAND>

Commands:
  add     Make people or Groups Contributors of the Application: they can open it, deploy it, and change both of its lists
  remove  Take people or Groups off the Application's Contributors (one must be left)
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `vibe contributors add`

```text
Make people or Groups Contributors of the Application: they can open it, deploy it, and change both of its lists

Usage: vibe contributors add [OPTIONS] <--user <ID>|--email <ADDRESS>|--group <GROUP>> <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --user <ID>        A person, by their user id (vibe whoami shows it)
      --email <ADDRESS>  A person, by their email address: the one who signed in here with it
      --group <GROUP>    A Group, by its name
      --json             Print exactly one JSON object on standard output
  -h, --help             Print help
```

### `vibe contributors remove`

```text
Take people or Groups off the Application's Contributors (one must be left)

Usage: vibe contributors remove [OPTIONS] <--user <ID>|--email <ADDRESS>|--group <GROUP>> <APPLICATION>

Arguments:
  <APPLICATION>  The Application, by name

Options:
      --user <ID>        A person, by their user id (vibe whoami shows it)
      --email <ADDRESS>  A person, by their email address: the one who signed in here with it
      --group <GROUP>    A Group, by its name
      --json             Print exactly one JSON object on standard output
  -h, --help             Print help
```


## For Platform administrators

`vibe serve` is the program the image runs; `vibe setup` is run inside the installation's container: `docker compose exec vibe vibe setup`.

### `vibe serve`

```text
Serve Applications, each on <name>.<domain>, and the platform's API on api.<domain>. The platform database is named by VIBE_DATABASE_URL, the installation's key is VIBE_SECRET_KEY.

To serve HTTPS itself, with no reverse proxy before it, set both VIBE_TLS_CERT_FILE, the PEM file of the certificate for *.<domain> and its chain, and VIBE_TLS_KEY_FILE, the PEM file of its private key (RSA of 2048 to 4096 bits, ECDSA on P-256 or P-384, or Ed25519); they are read again when they change. Without them it serves plain HTTP.

Usage: vibe serve [OPTIONS] --domain <DOMAIN> --listen <LISTEN>

Options:
      --domain <DOMAIN>
          The installation's domain

      --listen <LISTEN>
          The address to listen on; port 0 picks a free one

      --stand-in-sign-in
          Development only: the caller states the User in x-vm-stand-in-* headers

      --administrators-group <ADMINISTRATORS_GROUP>
          With the stand-in only: the Group whose members are Platform administrators. With OIDC the connection names it (vibe admin connect-oidc)

      --tool-credential-lifetime <TOOL_CREDENTIAL_LIFETIME>
          How long a tool credential lives from its sign-in, in seconds
          
          [default: 2592000]

      --public-scheme <PUBLIC_SCHEME>
          How a browser reaches the installation: https (the default) or http
          
          [default: https]
          [possible values: https, http]

      --public-port <PUBLIC_PORT>
          The port a browser reaches the installation at: 443 with https, the listening port with http, without it

      --session-renewal <SESSION_RENEWAL>
          How long the Groups read at sign-in are used before the identity provider is asked again, in seconds
          
          [default: 600]

      --session-lifetime <SESSION_LIFETIME>
          How long a session lives from its sign-in, in seconds
          
          [default: 43200]

  -h, --help
          Print help (see a summary with '-h')
```

### `vibe setup`

```text
Print the setup credential, good for 24 hours for connecting OIDC once (vibe admin connect-oidc). The platform database is named by VIBE_DATABASE_URL

Usage: vibe setup [OPTIONS]

Options:
      --json  Print exactly one JSON object on standard output
  -h, --help  Print help
```

### `vibe admin connect-oidc`

```text
Connect OIDC, with the setup credential or as a Platform administrator signed in.

With the setup credential in VIBE_SETUP_CREDENTIAL, --domain says where the installation is: over HTTPS, with --port and --resolve as for vibe sign-in, or over plain HTTP on this machine with --connect. Without it, the command goes to the installation the tool is signed in to. The client secret is read from VIBE_OIDC_CLIENT_SECRET.

Usage: vibe admin connect-oidc [OPTIONS] --issuer <ISSUER> --client-id <CLIENT_ID> --administrators-group <ADMINISTRATORS_GROUP>

Options:
      --domain <DOMAIN>
          With the setup credential: the installation's domain

      --port <PORT>
          The port of HTTPS (443 when not given)

      --resolve <RESOLVE>
          Connect to this IP address (IPv4 or IPv6, written alone) in place of what the system resolves api.<domain> to; nothing else changes: the certificate is checked for api.<domain>

      --connect <CONNECT>
          Plain HTTP instead, to an installation on this machine: a loopback address and port, such as 127.0.0.1:8080 or [::1]:8080

      --issuer <ISSUER>
          The identity provider's issuer, https://... (http:// only on a loopback host)

      --client-id <CLIENT_ID>
          The platform's client id at the identity provider

      --administrators-group <ADMINISTRATORS_GROUP>
          The Group whose members are Platform administrators

      --groups-claim <GROUPS_CLAIM>
          The ID token's claim that holds the person's Groups (groups without it)

      --scopes <SCOPES>
          The scopes asked for, separated by spaces; openid is always asked

      --json
          Print exactly one JSON object on standard output

  -h, --help
          Print help (see a summary with '-h')
```


## Every command and the exit codes

```text
Usage: vibe [COMMAND]

Commands:
  serve         Serve Applications, each on <name>.<domain>, and the platform's API on api.<domain>. The platform database is named by VIBE_DATABASE_URL, the installation's key is VIBE_SECRET_KEY
  setup         Print the setup credential, good for 24 hours for connecting OIDC once (vibe admin connect-oidc). The platform database is named by VIBE_DATABASE_URL
  admin         Commands for Platform administrators: connect-oidc
  sign-in       Sign the tool in to an installation, and keep its credential in your own configuration folder
  sign-out      End the tool's credential at the installation and remove it from this machine
  whoami        Who the tool is signed in as: the User, their Groups, the installation
  resources     Every registered Resource: its description, your Access level, and whom to ask
  applications  The Applications you contribute to and those shared with you: name, description, address, and which is which
  show          An Application: its address, serving Version, Contributors and whom it is shared with
  share         Share the Application: let people, Groups or everyone signed in open it (--user, --email, --group, --everyone; each as often as needed)
  unshare       Stop sharing the Application with people, Groups or everyone
  contributors  Change who maintains the Application: vibe contributors add|remove
  pull          Pull a Resource's structure into the project, as a typed Drizzle definition (resources/<name>/schema.ts) and its link (resources/<name>/index.ts)
  new           Make a new project from the template: an Application that answers one page, ready for npm install and vibe check
  check         Check whether the project will deploy: its Manifest, its Resources, its migrations and its types, every problem with its fix
  generate      Generate the next migration from src/db/schema.ts into migrations/, stopping before one that destroys data
  deploy        Deploy the project: build it on this machine from its last commit, check it, and send it as the Application's new Version, which goes live at its address
  versions      The Application's Versions: each one's commit, when and by whom it was deployed, and which one serves
  go-back       Serve an earlier Version again, at once: the one before the serving Version, or --to <number>. Migrations are not undone
  logs          What the Application printed and the errors it did not catch, request by request; --follow prints new entries as they come
  request       Run the project on this machine, beside a private Postgres with an empty stand-in of each pulled Resource, and send it one request as a test User of vibe.dev.json (--as builder, --as outsider, ...)
  dev           Keep the project running on this machine and serve it to your browser at http://<name>.localhost:7480/, as the test User you choose there; rebuilt when the project changes, until interrupted
  local         The project's local runs: what they recorded (operation-log, change-history), and reset, which starts the local databases over
  help          Print this message or the help of the given subcommand(s)

Options:
  -V, --version  Print the version and the execution core, and exit
  -h, --help     Print help

Exit codes:
  0  done
  1  failed: the installation cannot be reached, it answered something unexpected, or a file cannot be read or written
  2  the command line is wrong
  3  refused, or waiting for the Builder: the platform said no, or the next step is a person's (the message says whose and what)
  4  not signed in (no credential, or one that expired or was signed out): run vibe sign-in, then the command again

With --json, standard output is one JSON object; a command that does not end with 0 prints { "error": { "reason", "message" } } there.
```
