# steveql-fmt

An opinionated SQL formatter that follows Steve's SQL format. Do you know
Steve? Probably not. But if you knew him, you'd know that he knows that his
SQL format is the best. And he's right. So here's a tool to format SQL the
way he likes.

It lays statements out in a leading-comma, tab-grid "river" style. It parses
with [`sqlparser`](https://github.com/apache/datafusion-sqlparser-rs)
(PostgreSQL dialect) and applies its own layout.

It gives the full treatment to `SELECT` / `WITH`, `MERGE`, `INSERT`, `UPDATE`,
and `DELETE`, and to the query inside `CREATE TABLE ... AS` and `CREATE VIEW`.
Other statement kinds (DDL, etc.) are passed through as compact single-line SQL.

## Usage

```sh
# format a file to stdout
steveql-fmt query.sql

# rewrite files in place
steveql-fmt --write query.sql other.sql

# fail (non-zero exit) if anything isn't already formatted
steveql-fmt --check query.sql

# read stdin, write stdout
cat query.sql | steveql-fmt

# run the formatting language server (see Editor integration)
steveql-fmt lsp
```

The input is split into statements, and each one is formatted independently.
Any statement that can't be parsed (e.g. a SQLMesh `MODEL (...)` block) is
passed through **unchanged** with a warning on stderr — so a SQLMesh model file
gets its query formatted while the `MODEL` block is left intact, and the tool
never garbles SQL it doesn't understand. Exit code is 0.

The same goes for a statement the layout can't fully represent. Each formatted
statement is parsed again and compared with the original; if anything was lost
(a clause the formatter has no place for, such as MySQL's `DELETE ... LIMIT`),
the statement is left exactly as written, with a warning, rather than changed.

With `mise`:

```sh
mise run build              # cargo build
mise run run -- query.sql   # cargo run -- query.sql
mise run format             # cargo fmt
mise run test               # cargo test
mise run lint               # format + clippy
mise run lsp                # run the language server over stdio
mise run examples           # reformat the example SQL files in place
```

## Editor integration

Because the CLI reads stdin and writes stdout (and passes unparseable statements
through unchanged), it plugs into most editors' "external formatter on save"
hook directly — anything it can't parse is simply left as-is:

```toml
# Helix — languages.toml
[[language]]
name = "sql"
formatter = { command = "steveql-fmt" }
auto-format = true
```

```lua
-- Neovim — conform.nvim
require("conform").setup({
  formatters = { steveql = { command = "steveql-fmt", stdin = true } },
  formatters_by_ft = { sql = { "steveql" } },
  format_on_save = true,
})
```

```jsonc
// Zed — settings.json
{
  "languages": {
    "SQL": {
      "formatter": { "external": { "command": "steveql-fmt", "arguments": [] } },
      "format_on_save": "on"
    }
  }
}
```

Vim has it built in: `:setlocal formatprg=steveql-fmt`, then `gq`.

### Language server

`steveql-fmt lsp` runs a formatting-only language server over stdio (it
implements `textDocument/formatting`). Point any LSP client at it for SQL — for
example in Neovim:

```lua
vim.lsp.start({
    name = "steveql-fmt",
    cmd = { "steveql-fmt", "lsp" },
    filetypes = { "sql" }
})
-- then: vim.lsp.buf.format()  (or format-on-save via an autocmd)
```

It tracks open documents with full sync and returns a whole-document edit; a
parse error yields no edits, so the buffer is left as-is.

## The style

The core idea: indentation lands on a **4-column tabstop** (or one space past
it, for the leading-comma columns), so the layout is reproducible by hand with a
tab key. Each aligned field begins at the next multiple of four that leaves at
least one space — `ceil4(prev_field_last_col + 2)`. Values flow in a vertical
"river" with the comma sitting one column to their left.

```sql
WITH RECURSIVE roots AS (
        SELECT   c.oid
                ,n.nspname
        FROM    pg_class        c
        JOIN    pg_namespace    n   ON  n.oid = c.relnamespace
        WHERE   c.relkind IN ('r', 'p')
                AND NOT c.relispartition
    )
SELECT   r.nspname
        ,COUNT(*)   AS n
FROM    roots   r
ORDER BY 1, 2
```

### SELECT / WITH

- **Projection:** one item per line, leading commas. `AS` aligns to the tabstop
  past the widest value in the list, and the alias name snaps to the next
  tabstop past `AS`. Block items (subqueries, `EXISTS`, window calls, `CASE`)
  align on the same grid — their value "ends" at the closing `)` (or `END`).
- **FROM and its JOINs** form one alignment group: table names, aliases, `ON`,
  and the join condition each get their own grid column; trailing `AND`/`OR`
  align under `ON`. `LEFT JOIN` (etc.) widens the group, pushing tables out, and
  so does `LATERAL`, which joins the keyword (`CROSS JOIN LATERAL`).
  Comma-separated tables take leading commas like a SELECT list. `TABLESAMPLE`
  follows the alias on the next tabstop, or takes its column when there's none.
- **WHERE / HAVING** put the keyword in the gutter and flow predicates in the
  river, with `AND`/`OR` on the river. A parenthesized `AND`/`OR` operand
  expands into a nested sub-river: `(` opens at the operand tabstop, the inner
  chain rivers there, and `)` closes back at the outer river.
- **GROUP BY / ORDER BY** stay inline when every item is a column ordinal
  (`ORDER BY 1, 2`); as soon as a column *name* appears they break to one item
  per line with leading commas.
- **Window functions** expand `OVER (...)` into an aligned `PARTITION BY` /
  `ORDER BY` block, with a frame's `ROWS` / `RANGE` / `GROUPS` in the keyword
  column and a base window's name on the first line. The `WINDOW` clause lays
  each definition out the same way.
- **Subqueries, windows, and `CASE`** expand into indented blocks wherever they
  sit: a SELECT item, a `WHERE` operand (`total = (`, `id IN (`, `EXISTS (`), a
  `SET` or `VALUES` value, the lone argument of a call (`SUM(CASE`), and so on,
  recursively. The text around the block stays inline on its opening and closing
  lines (`END) > 0`, `) = 1`). A call with several arguments, one of them
  holding a block, puts one argument per line, a tabstop in with leading commas,
  and closes its `)` back at the call's column. A short subquery, one item from at most one table and
  nothing else, stays inline: `(SELECT MAX(id) FROM s)`.
- **Derived tables** (subqueries in `FROM`/`JOIN`) put the keyword alone on its
  line; `(` drops to the next line at the keyword's indent, the body indents one
  level past that, and `)` returns to the keyword's indent and carries the alias
  / `ON` clause at the shared FROM/JOIN grid columns. (This keeps deeply nested
  subqueries from marching off the right margin.)
- **`CASE`** expands to a block: `WHEN`/`ELSE` indent one tabstop in from the
  value column's tabstop, and `END` returns to the value column so it lines up
  with `CASE`.
- **Set operations** put a blank line between each query and the operator. A
  parenthesized query keeps its parentheses, laid out like a derived table, and
  needs no blank line on that side.
- **`CREATE TABLE ... AS` / `CREATE VIEW ... AS`** keep everything before `AS`
  on one line, put `AS` on its own line, and lay out the query below it.

### MERGE / INSERT / UPDATE / DELETE

The DML statements reuse the same building blocks (the FROM/JOIN grid, the WHERE
river, leading-comma lists). Parenthesized column / `VALUES` lists "hug": `(`
glues to the first item, `)` to the last, with leading commas under the `(`.
A single `VALUES` row gets one value per line; several rows get one row per line
in a leading-comma list. `RETURNING` sits alone on its line with its items one
tabstop in, and `ON CONFLICT ... DO UPDATE` lays out like a `MERGE` action. A
lone `SET` assignment drops the leading-comma column and the `=` alignment.

```sql
MERGE
INTO    warehouse.inventory t
USING
(
    SELECT   sku        AS  sku
            ,SUM(qty)   AS  qty
    FROM    staging.receipts
    GROUP BY sku
)                           s   ON  t.sku = s.sku
WHEN MATCHED THEN
    UPDATE SET
         qty        = t.qty + s.qty
        ,updated_at = NOW()
WHEN NOT MATCHED THEN
    INSERT
        (sku
        ,qty
        ,updated_at)
    VALUES
        (s.sku
        ,s.qty
        ,NOW())
```

```sql
INSERT INTO "schema"."t"
    ("center_id"
    ,"staff_id")
VALUES
    ('c-123'
    ,'s-456')
```

```sql
UPDATE  "schema"."t"
SET      "note"         = 'updated'
        ,"is_released"  = TRUE
WHERE   "id" = '...'
        AND "center_id" = 'c-123'
```

```sql
DELETE
FROM    "schema"."t"
WHERE   "is_released" <> TRUE  -- 🤮
        AND "updated_at" < '2026-01-01'
```

- **MERGE:** `MERGE` heads its own line, and the target (`INTO`) and source
  (`USING`) form one aligned relation group — the source alias lines up under
  the target alias, with `USING` laid out like a joined derived table (its
  closing paren carries the alias and the `ON` clause). Each `WHEN … THEN` sits
  on its own line with the action (`UPDATE SET` / `INSERT` / `VALUES` /
  `DELETE`) indented below it.
- **UPDATE:** the table sits on the `UPDATE` line at the clause column; `SET`
  leads the assignment list, with `=` aligned to the tabstop past the widest
  target.
- **DELETE:** `DELETE` on its own line, the target laid out like a `FROM`.

## Casing

- Reserved keywords are upper-cased (`SELECT`, `AND`, `IN`, `IS NOT NULL`, …).
- Data types are upper-cased (`::BIGINT`).
- `TRUE` / `FALSE` / `NULL` are upper-cased.
- **Standard SQL functions** are upper-cased (`COUNT`, `SUM`, `COALESCE`,
  `ROW_NUMBER`, …). The list lives in [`src/funcs.rs`](src/funcs.rs) — edit
  `STANDARD_FUNCTIONS` to taste.
- Everything else (table/column names, Postgres functions like
  `pg_table_size`, your own helpers) keeps the casing you wrote.
- The not-equal operator keeps the spelling you wrote (`!=` stays `!=`, `<>`
  stays `<>`), but every `<>` gets a trailing `-- 🤮` to register that `!=`
  would have been nicer. (Some dialects only accept `<>`, so it isn't forced.)

## Placeholders

psycopg-style placeholders (`%s`, `%(name)s`) are not valid SQL, so they are
swapped for parser-safe tokens before parsing and restored verbatim in the
output.

## Comments

`sqlparser` discards comments during parsing, so they are collected separately
(with source line numbers) and re-attached by position: standalone comments
become their own lines and trailing comments stay on the line they followed.
Placement is best-effort — a comment buried deep inside an expression may be
nudged to the nearest clause boundary — but no comment is ever dropped.

## Development

`examples/` holds one already-formatted file per formatting rule (named after
the rule it shows off, e.g. `joins.sql`, `window-functions.sql`). They double as
the test corpus: `mise run test` asserts that formatting each one is a no-op, so
every example is guaranteed to be in canonical form (and the formatter is proven
idempotent over the whole set).

To add or change behavior, edit an example to the exact layout you want, adjust
the formatter until `steveql-fmt examples/that-file.sql` reproduces it, and the
test will hold you to it. `mise run examples` reformats the examples in place.

## Known limitations

- Statement kinds other than `SELECT`/`WITH`/`MERGE`/`INSERT`/`UPDATE`/`DELETE`
  (DDL and friends) fall back to compact single-line SQL.
- Subqueries expand as blocks when they're a SELECT item, a `FROM`/`USING`
  table, an `EXISTS`, or a top-level boolean operand (including inside a
  parenthesized `AND`/`OR`). A subquery buried inside another expression — e.g.
  `col = (SELECT …)`, `col IN (SELECT …)`, or a function argument — still renders
  inline.
- Statements are split on top-level `;`. A statement that can't be parsed (a
  SQLMesh `MODEL (...)` block, DDL, etc.) is passed through verbatim with a
  warning, while the rest of the file is still formatted. Semicolons inside
  dollar-quoted strings (`$$ … $$`) aren't recognized as boundaries.

These are the natural places to extend next — send a tricky example and the
style can be pinned down the same way the rest was.
