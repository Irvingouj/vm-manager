# Every database has exactly one schema owner

Builders and Coding agents write queries the same typed way against every database, but a database's structure is changed by one owner only. An Application owns the structure of its own database: the definition lives as TypeScript in its project, migrations are generated from it, and the platform applies them on deploy with a privileged role, so the Application holds no right to change structure at runtime. A shared database is owned outside the Application (by IT or the team that already runs it): its structure is pulled from the platform as a read-only typed definition, and no Application can change it. We chose this because a Drizzle-style workflow assumes the code repository is the single source of truth for structure, and a database many Applications use cannot have many sources of truth.

Pulled definitions go stale, so there are two checks. A deploy pulls the current structure again and type-checks the Application against it; it fails only if the Application's code no longer compiles, not merely because something changed. And the platform records which structure each deployed Application was built against, so that when a shared database changes later it can report the deployed Applications that reference what no longer exists.

## Considered Options

- **Each Application carries its own definition of a shared database and migrates it**: Applications overwrite each other.
- **The platform owns shared structure through a reviewed schema project**: reasonable, and on the roadmap, but Milestone 1 registers databases that already exist and are already managed by someone.

## Consequences

- An Application that wants one more column on a shared table cannot add it. It keeps the extra data in its own database and relates the two in code: there are no joins across an Application's own database and a shared one.
- The deploy check protects only the Application being deployed, and only code that goes through the typed query path. Raw SQL is not covered. Applications already running are covered by the report, which in Milestone 1 informs and does not block.
