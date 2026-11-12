// A deliberately minimal stand-in for the query surface, which is a different
// map ticket. Only here so that handles can show Access levels.

export type Row = Record<string, unknown>;

/** Raw SQL. The types cannot know whether it writes; the host must (read-only Postgres role). */
export interface RawSql {
  readonly text: string;
  readonly params: readonly unknown[];
}

export interface ReadOps {
  select<T extends Row = Row>(table: string, where?: Partial<T>): Promise<T[]>;
  query<T extends Row = Row>(sql: RawSql): Promise<T[]>;
}

export interface WriteOps {
  insert<T extends Row>(table: string, row: T): Promise<{ rowCount: number }>;
  update<T extends Row>(table: string, where: Partial<T>, set: Partial<T>): Promise<{ rowCount: number }>;
}
