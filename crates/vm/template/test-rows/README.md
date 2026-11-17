# Test rows

SQL files whose rows the local runs (`vm request`, `vm dev`) load, on this machine only. They are never sent anywhere, and a deploy does not read them.

- `<resource>.sql`: rows for the stand-in of a pulled Resource, named as the Resource (`devops.sql` for `vm pull devops`). Before it is loaded, every table of the stand-in is emptied.
- `own-database.sql`: rows for the Application's own database, loaded after its migrations, as the Application itself could write them.

Write `INSERT` statements for the most part, for example:

```sql
INSERT INTO incidents (id, title) VALUES (1, 'Printer on fire');
```

A file is loaded in one transaction when it changes; a statement that fails loads nothing of its file and says which one. `vm local reset` starts the local databases over and loads every file again. A file is at most 16 MiB.
