//! DDL layouts: `CREATE TABLE`, `CREATE SECRET`, and `ALTER TABLE`.
//!
//! Each statement is a header line followed by a list of items with leading
//! commas. An item is a few aligned parts (a column's name, type, nullability,
//! and constraints; an ALTER action's keyword, object, and clauses), and its
//! last part may be several clauses stacked one per line, lined up under the
//! first the way `AND` lines up under `ON`.

use super::{align_after, pad, pad_to, Fmt};
use sqlparser::ast::{
    AlterColumnOperation, AlterTable, AlterTableOperation, ColumnDef, ColumnOption, CreateTable,
    Expr, ForeignKeyConstraint, Ident, RenameTableNameKind, SecretOption, TableConstraint,
};

/// One item of a list: its aligned leading parts, then clauses stacked at the
/// column after the last part.
struct Item {
    parts: Vec<String>,
    clauses: Vec<String>,
}

impl Fmt {
    /// `CREATE TABLE name (` with one column per line and the table
    /// constraints after a blank line, then `)` and any trailing options.
    /// `None` when the table can't be laid out that way (no column list, or a
    /// `LIKE` / `CLONE` / `PARTITION OF` in its place).
    pub(super) fn create_table(&self, ct: &CreateTable) -> Option<Vec<String>> {
        if ct.columns.is_empty() && ct.constraints.is_empty() {
            return None;
        }
        // The header and trailing options are whatever renders around an
        // emptied column list.
        let mut bare = ct.clone();
        bare.columns.clear();
        bare.constraints.clear();
        let text = self.post(&bare.to_string());
        let (header, trailing) = text.split_once(" ()")?;
        let trailing = match &ct.partition_by {
            Some(p) => trailing.replacen(&self.post(&p.to_string()), &self.call_spaced(p), 1),
            None => trailing.to_string(),
        };

        let columns: Vec<Item> = ct.columns.iter().map(|c| self.column_item(c)).collect();
        let constraints: Vec<(Option<String>, Vec<String>)> = ct
            .constraints
            .iter()
            .map(|c| self.constraint_clauses(c))
            .collect();

        let multi = columns.len() + constraints.len() > 1;
        let comma_col = 4;
        let content = if multi { comma_col + 1 } else { comma_col };
        let mut rows = self.aligned_items(&columns, content);
        if !columns.is_empty() && !constraints.is_empty() {
            rows.push(Vec::new()); // a blank line before the table constraints
        }
        rows.extend(self.constraint_rows(&constraints, content));

        let mut out = vec![format!("{header} (")];
        out.extend(lead_commas(rows, comma_col, content));
        out.push(")".to_string());
        let trailing = trailing.trim();
        if !trailing.is_empty() {
            out.push(trailing.to_string());
        }
        Some(out)
    }

    /// `CREATE SECRET name (` with `TYPE` and each option on its own line,
    /// names and values aligned, then `)`.
    pub(super) fn create_secret(
        &self,
        stmt: &sqlparser::ast::Statement,
        secret_type: &Ident,
        options: &[SecretOption],
    ) -> Option<Vec<String>> {
        let text = stmt.to_string();
        let (header, _) = text.split_once(" (")?;
        let mut items = vec![Item {
            parts: vec!["TYPE".to_string(), secret_type.to_string()],
            clauses: vec![],
        }];
        items.extend(options.iter().map(|o| Item {
            parts: vec![o.key.value.to_uppercase(), o.value.to_string()],
            clauses: vec![],
        }));
        let comma_col = 4;
        let content = if items.len() > 1 {
            comma_col + 1
        } else {
            comma_col
        };
        let mut out = vec![format!("{} (", self.post(header))];
        out.extend(lead_commas(
            self.aligned_items(&items, content),
            comma_col,
            content,
        ));
        out.push(")".to_string());
        Some(out)
    }

    /// `ALTER TABLE name` with one action per line below it: the action's
    /// keyword, the object it acts on, and its clauses, each aligned.
    pub(super) fn alter_table(&self, at: &AlterTable) -> Option<Vec<String>> {
        if at.location.is_some() || at.operations.is_empty() {
            return None;
        }
        let mut bare = at.clone();
        bare.operations.clear();
        let header = self.post(bare.to_string().trim_end());
        let items: Vec<Item> = at
            .operations
            .iter()
            .map(|op| self.action_item(op))
            .collect();
        let comma_col = 4;
        let content = if items.len() > 1 {
            comma_col + 1
        } else {
            comma_col
        };
        let mut out = vec![header];
        out.extend(lead_commas(
            self.aligned_items(&items, content),
            comma_col,
            content,
        ));
        Some(out)
    }

    /// Lay out items whose parts share columns: each part starts on the
    /// tabstop past the widest earlier part, and the clauses on the tabstop
    /// past the widest last part. Every item counts toward the last part's
    /// width, but an earlier part only counts items with something after it
    /// (a column with no options doesn't push the nullability column out). An
    /// empty part (a column with no nullability) leaves its column blank. Each item's lines start at `content`; the first
    /// comes back without its indent so a leading comma can be placed.
    fn aligned_items(&self, items: &[Item], content: usize) -> Vec<Vec<String>> {
        let n_parts = items.iter().map(|i| i.parts.len()).max().unwrap_or(0);
        // cols[k] is where part k starts; cols[n_parts] is the clause column.
        let mut cols = vec![content];
        for k in 0..n_parts {
            let last = k + 1 == n_parts;
            let widest = items
                .iter()
                .filter(|i| last || i.parts.len() > k + 1 || !i.clauses.is_empty())
                .filter_map(|i| i.parts.get(k))
                .map(|p| p.chars().count())
                .max()
                .unwrap_or(0);
            let col = cols[k];
            cols.push(if widest == 0 {
                col
            } else {
                align_after(col + widest - 1)
            });
        }
        items
            .iter()
            .map(|item| {
                let mut line = pad(content);
                for (k, part) in item.parts.iter().enumerate() {
                    if !part.is_empty() {
                        pad_to(&mut line, cols[k]);
                        line.push_str(part);
                    }
                }
                let clause_col = if item.clauses.is_empty() {
                    0
                } else if item.parts.is_empty() {
                    content
                } else {
                    cols[item.parts.len()]
                };
                let mut lines = vec![line];
                for (i, clause) in item.clauses.iter().enumerate() {
                    if i == 0 {
                        pad_to(&mut lines[0], clause_col);
                        lines[0].push_str(clause);
                    } else {
                        lines.push(format!("{}{clause}", pad(clause_col)));
                    }
                }
                lines[0] = lines[0][content..].to_string();
                lines
            })
            .collect()
    }

    /// A column definition: name, type, and nullability as aligned parts
    /// (nullability only when it's the first option, so nothing moves), and
    /// every other option as a clause.
    fn column_item(&self, c: &ColumnDef) -> Item {
        let mut options = c.options.iter().peekable();
        let null = match options.peek().map(|o| (&o.name, &o.option)) {
            Some((None, ColumnOption::NotNull | ColumnOption::Null)) => {
                options.next().map(|o| o.option.to_string())
            }
            _ => None,
        };
        let mut clauses = Vec::new();
        for o in options {
            let mut opt = match &o.option {
                ColumnOption::ForeignKey(fk) => self.references_clauses(fk),
                other => vec![self.post(&other.to_string())],
            };
            if let Some(name) = &o.name {
                opt[0] = format!("CONSTRAINT {name} {}", opt[0]);
            }
            clauses.extend(opt);
        }
        let mut parts = vec![c.name.to_string(), self.data_type(&c.data_type)];
        if null.is_some() || !clauses.is_empty() {
            parts.push(null.unwrap_or_default());
        }
        Item { parts, clauses }
    }

    /// A data type, with a space after each comma (`NUMERIC(12, 2)`).
    fn data_type(&self, t: &sqlparser::ast::DataType) -> String {
        space_commas(&self.post(&t.to_string()))
    }

    /// A table constraint's name (if any) and its clauses.
    fn constraint_clauses(&self, c: &TableConstraint) -> (Option<String>, Vec<String>) {
        let mut bare = c.clone();
        let name = match &mut bare {
            TableConstraint::Unique(u) => u.name.take(),
            TableConstraint::PrimaryKey(p) => p.name.take(),
            TableConstraint::ForeignKey(f) => f.name.take(),
            TableConstraint::Check(k) => k.name.take(),
            _ => None,
        };
        let clauses = match &bare {
            TableConstraint::ForeignKey(fk) => {
                let mut head = "FOREIGN KEY".to_string();
                if let Some(index) = &fk.index_name {
                    head.push_str(&format!(" {index}"));
                }
                let cols: Vec<String> = fk.columns.iter().map(|c| c.to_string()).collect();
                head.push_str(&format!(" ({})", cols.join(", ")));
                let mut clauses = vec![head];
                clauses.extend(self.references_clauses(fk));
                clauses
            }
            other => vec![self.post(&other.to_string())],
        };
        (name.map(|n| n.to_string()), clauses)
    }

    /// Table constraint rows: a named one is `CONSTRAINT` / name / clauses on
    /// a shared grid, like an ALTER action; an unnamed one stacks its clauses
    /// at the item's column.
    fn constraint_rows(
        &self,
        constraints: &[(Option<String>, Vec<String>)],
        content: usize,
    ) -> Vec<Vec<String>> {
        let named: Vec<Item> = constraints
            .iter()
            .filter_map(|(name, clauses)| {
                name.as_ref().map(|n| Item {
                    parts: vec!["CONSTRAINT".to_string(), n.clone()],
                    clauses: clauses.clone(),
                })
            })
            .collect();
        let mut named = self.aligned_items(&named, content).into_iter();
        constraints
            .iter()
            .map(|(name, clauses)| match name {
                Some(_) => named.next().unwrap_or_default(),
                None => self
                    .aligned_items(
                        &[Item {
                            parts: vec![],
                            clauses: clauses.clone(),
                        }],
                        content,
                    )
                    .remove(0),
            })
            .collect()
    }

    /// `REFERENCES table (cols)` and each referential action after it.
    fn references_clauses(&self, fk: &ForeignKeyConstraint) -> Vec<String> {
        let mut refs = format!("REFERENCES {}", fk.foreign_table);
        if !fk.referred_columns.is_empty() {
            let cols: Vec<String> = fk.referred_columns.iter().map(|c| c.to_string()).collect();
            refs.push_str(&format!(" ({})", cols.join(", ")));
        }
        if let Some(m) = &fk.match_kind {
            refs.push_str(&format!(" {m}"));
        }
        let mut clauses = vec![self.post(&refs)];
        if let Some(a) = &fk.on_delete {
            clauses.push(format!("ON DELETE {a}"));
        }
        if let Some(a) = &fk.on_update {
            clauses.push(format!("ON UPDATE {a}"));
        }
        if let Some(c) = &fk.characteristics {
            clauses.push(c.to_string());
        }
        clauses
    }

    /// An ALTER TABLE action as keyword / object / clauses.
    fn action_item(&self, op: &AlterTableOperation) -> Item {
        let item = |kw: String, object: String, clauses: Vec<String>| Item {
            parts: vec![kw, object],
            clauses,
        };
        match op {
            AlterTableOperation::AddColumn {
                column_keyword,
                if_not_exists,
                column_def,
                column_position,
            } => {
                let mut kw = if *column_keyword { "ADD COLUMN" } else { "ADD" }.to_string();
                if *if_not_exists {
                    kw.push_str(" IF NOT EXISTS");
                }
                // The definition after the name stacks: type, then each option.
                let col = self.column_item(column_def);
                let mut clauses = vec![col.parts[1].clone()];
                clauses.extend(col.parts.get(2).filter(|n| !n.is_empty()).cloned());
                clauses.extend(col.clauses);
                clauses.extend(column_position.iter().map(|p| p.to_string()));
                item(kw, col.parts[0].clone(), clauses)
            }
            AlterTableOperation::DropColumn {
                has_column_keyword,
                column_names,
                if_exists,
                drop_behavior,
            } => {
                let mut kw = if *has_column_keyword {
                    "DROP COLUMN"
                } else {
                    "DROP"
                }
                .to_string();
                if *if_exists {
                    kw.push_str(" IF EXISTS");
                }
                let names: Vec<String> = column_names.iter().map(|c| c.to_string()).collect();
                let clauses = drop_behavior.iter().map(|b| b.to_string()).collect();
                item(kw, names.join(", "), clauses)
            }
            AlterTableOperation::AlterColumn { column_name, op } => {
                let clauses = match op {
                    AlterColumnOperation::SetDataType {
                        data_type,
                        using,
                        had_set,
                    } => {
                        let kw = if *had_set { "SET DATA TYPE" } else { "TYPE" };
                        let mut clauses = vec![format!("{kw} {}", self.data_type(data_type))];
                        if let Some(u) = using {
                            clauses.push(format!("USING {}", self.inline(u)));
                        }
                        clauses
                    }
                    other => vec![self.post(&other.to_string())],
                };
                item("ALTER COLUMN".to_string(), column_name.to_string(), clauses)
            }
            AlterTableOperation::AddConstraint {
                constraint,
                not_valid,
            } => {
                let (name, mut clauses) = self.constraint_clauses(constraint);
                if *not_valid {
                    clauses.push("NOT VALID".to_string());
                }
                match name {
                    Some(n) => item("ADD CONSTRAINT".to_string(), n, clauses),
                    None => Item {
                        parts: vec!["ADD".to_string()],
                        clauses,
                    },
                }
            }
            AlterTableOperation::DropConstraint {
                if_exists,
                name,
                drop_behavior,
            } => {
                let mut kw = "DROP CONSTRAINT".to_string();
                if *if_exists {
                    kw.push_str(" IF EXISTS");
                }
                let clauses = drop_behavior.iter().map(|b| b.to_string()).collect();
                item(kw, name.to_string(), clauses)
            }
            AlterTableOperation::RenameColumn {
                old_column_name,
                new_column_name,
            } => item(
                "RENAME COLUMN".to_string(),
                old_column_name.to_string(),
                vec![format!("TO {new_column_name}")],
            ),
            AlterTableOperation::RenameTable { table_name } => match table_name {
                RenameTableNameKind::To(n) => item("RENAME TO".to_string(), n.to_string(), vec![]),
                RenameTableNameKind::As(n) => item("RENAME AS".to_string(), n.to_string(), vec![]),
            },
            AlterTableOperation::OwnerTo { new_owner } => {
                item("OWNER TO".to_string(), new_owner.to_string(), vec![])
            }
            other => Item {
                parts: vec![self.post(&other.to_string())],
                clauses: vec![],
            },
        }
    }

    /// A call written with a space before its arguments and its name
    /// upper-cased, like `RANGE (viewed_at)`; anything else renders inline.
    fn call_spaced(&self, e: &Expr) -> String {
        match e {
            Expr::Function(f) => {
                let args = f.args.to_string();
                let args = args.trim_start_matches('(').trim_end_matches(')');
                let name = f.name.to_string().to_uppercase();
                format!("{name} ({})", self.post(args))
            }
            other => self.inline(other),
        }
    }
}

/// Put item lines under a leading-comma skeleton: the first item at `content`
/// (`comma_col` + 1 when there are several), later ones after a comma at
/// `comma_col`. An empty item is a blank line.
fn lead_commas(items: Vec<Vec<String>>, comma_col: usize, content: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut first = true;
    for item in items {
        let Some((head, rest)) = item.split_first() else {
            out.push(String::new());
            continue;
        };
        let lead = if first {
            pad(content)
        } else {
            format!("{},", pad(comma_col))
        };
        first = false;
        out.push(format!("{lead}{head}"));
        out.extend(rest.iter().cloned());
    }
    out
}

/// Insert a space after any comma that lacks one.
fn space_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == ',' && chars.peek().is_some_and(|n| *n != ' ') {
            out.push(' ');
        }
    }
    out
}
