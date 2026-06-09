//! The opinionated block formatter.
//!
//! sqlparser handles parsing; this module decides layout. Inline expressions
//! are rendered with sqlparser's `Display` (then post-processed for
//! placeholders and standard-function casing), while the clause structure
//! (SELECT list, FROM/JOIN, WHERE river, CTEs, subqueries, window specs) is
//! laid out by hand on a 4-column alignment grid.

use crate::funcs;
use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, Cte, Delete, Distinct, Expr, FromTable,
    GroupByExpr, Insert, Join, JoinConstraint, JoinOperator, Merge, MergeAction, MergeClause,
    MergeClauseKind, MergeInsertKind, OrderBy, OrderByExpr, OrderByKind, Query, Select, SelectItem,
    SetExpr, SetOperator, SetQuantifier, Spanned, Statement, TableFactor, TableObject,
    TableWithJoins, Update, Values, WindowType,
};
use std::cell::Cell;

/// A source comment to be re-attached during formatting.
pub struct Comment {
    /// 1-based source line where the comment begins.
    pub line: u64,
    /// True if the comment trailed code on the same line.
    pub trailing: bool,
    /// Rendered comment text including its `--` or `/* */` delimiters.
    pub text: String,
}

pub struct Fmt {
    /// Original psycopg placeholders, indexed by sentinel number.
    placeholders: Vec<String>,
    /// Comments sorted by source position, consumed via `cursor`.
    comments: Vec<Comment>,
    cursor: Cell<usize>,
}

impl Fmt {
    pub fn new(placeholders: Vec<String>, comments: Vec<Comment>) -> Self {
        Fmt {
            placeholders,
            comments,
            cursor: Cell::new(0),
        }
    }

    pub fn statement(&self, stmt: &Statement) -> String {
        let mut out = Vec::new();
        // Comments before the statement (e.g. a header). A standalone comment
        // on the same line as the statement (it has no code before it) is still
        // a leading comment, so emit it too.
        let start = stmt.span().start.line;
        {
            let mut cur = self.cursor.get();
            while let Some(c) = self.comments.get(cur) {
                if c.line < start || (c.line == start && !c.trailing) {
                    out.push(c.text.clone());
                    cur += 1;
                } else {
                    break;
                }
            }
            self.cursor.set(cur);
        }
        match stmt {
            Statement::Query(q) => out.extend(self.query(q, 0)),
            Statement::Merge(m) => out.extend(self.merge(m, 0)),
            Statement::Insert(i) => out.extend(self.insert(i, 0)),
            Statement::Update(u) => out.extend(self.update(u, 0)),
            Statement::Delete(d) => out.extend(self.delete(d, 0)),
            // Anything we don't lay out by hand falls back to compact SQL.
            other => out.push(self.post(&other.to_string())),
        }
        // Any comments after the last anchored element (e.g. a trailing footer)
        // are emitted so nothing is ever dropped.
        self.flush_before(u64::MAX, 0, &mut out);
        out.join("\n")
    }

    // ---------- comment re-attachment ----------

    /// Emit, as standalone lines at indent `ci`, every pending comment whose
    /// source line precedes `line`.
    fn flush_before(&self, line: u64, ci: usize, out: &mut Vec<String>) {
        let mut cur = self.cursor.get();
        while cur < self.comments.len() && self.comments[cur].line < line {
            out.push(format!("{}{}", pad(ci), self.comments[cur].text));
            cur += 1;
        }
        self.cursor.set(cur);
    }

    /// Attach any trailing comment on `end_line` to the last emitted line.
    fn attach_trailing(&self, end_line: u64, out: &mut [String]) {
        let mut cur = self.cursor.get();
        while cur < self.comments.len()
            && self.comments[cur].trailing
            && self.comments[cur].line == end_line
        {
            if let Some(last) = out.last_mut() {
                last.push_str("  ");
                last.push_str(&self.comments[cur].text);
            }
            cur += 1;
        }
        self.cursor.set(cur);
    }

    // ---------- queries ----------

    fn query(&self, q: &Query, ci: usize) -> Vec<String> {
        let mut out = Vec::new();

        if let Some(with) = &q.with {
            self.render_with(with.recursive, &with.cte_tables, ci, &mut out);
        }

        out.extend(self.set_expr(&q.body, ci));

        if let Some(ob) = &q.order_by {
            out.extend(self.order_by(ob, ci));
        }

        if let Some(lc) = &q.limit_clause {
            out.extend(self.limit(lc, ci));
        }

        out
    }

    fn render_with(&self, recursive: bool, ctes: &[Cte], ci: usize, out: &mut Vec<String>) {
        let name_indent = ci + 4;
        let body_ci = ci + 8;
        for (i, cte) in ctes.iter().enumerate() {
            let name = cte.alias.name.to_string();
            self.flush_before(cte.query.span().start.line, name_indent, out);
            if i == 0 {
                let kw = if recursive { "WITH RECURSIVE" } else { "WITH" };
                out.push(format!("{}{} {} AS (", pad(ci), kw, name));
            } else {
                out.push(format!("{},{} AS (", pad(name_indent), name));
            }
            out.extend(self.query(&cte.query, body_ci));
            out.push(format!("{})", pad(name_indent)));
        }
    }

    fn set_expr(&self, body: &SetExpr, ci: usize) -> Vec<String> {
        match body {
            SetExpr::Select(s) => self.select(s, ci),
            SetExpr::Query(q) => self.query(q, ci),
            SetExpr::SetOperation {
                left,
                op,
                set_quantifier,
                right,
            } => {
                let mut out = self.set_expr(left, ci);
                out.push(format!("{}{}", pad(ci), set_op_str(op, set_quantifier)));
                out.extend(self.set_expr(right, ci));
                out
            }
            // A CTE in front of a DML statement (`WITH ... UPDATE/INSERT/...`)
            // parses as a Query whose body is the wrapped statement; lay it out
            // by hand rather than collapsing it to one inline line.
            SetExpr::Insert(Statement::Insert(i)) => self.insert(i, ci),
            SetExpr::Update(Statement::Update(u)) => self.update(u, ci),
            SetExpr::Delete(Statement::Delete(d)) => self.delete(d, ci),
            SetExpr::Merge(Statement::Merge(m)) => self.merge(m, ci),
            other => vec![format!("{}{}", pad(ci), self.post(&other.to_string()))],
        }
    }

    // ---------- SELECT ----------

    fn select(&self, s: &Select, ci: usize) -> Vec<String> {
        let mut out = self.projection(s, ci);
        if !s.from.is_empty() {
            out.extend(self.from(&s.from, ci));
        }
        if let Some(sel) = &s.selection {
            out.extend(self.river("WHERE", sel, ci));
        }
        if let GroupByExpr::Expressions(exprs, _) = &s.group_by {
            if !exprs.is_empty() {
                out.extend(self.group_by(exprs, ci));
            }
        }
        if let Some(having) = &s.having {
            out.extend(self.river("HAVING", having, ci));
        }
        out
    }

    fn projection(&self, s: &Select, ci: usize) -> Vec<String> {
        let kw = match &s.distinct {
            None => "SELECT".to_string(),
            Some(Distinct::On(exprs)) => {
                let list = exprs.iter().map(|e| self.inline(e)).collect::<Vec<_>>();
                format!("SELECT DISTINCT ON ({})", list.join(", "))
            }
            // Distinct / All / any other quantifier.
            Some(_) => "SELECT DISTINCT".to_string(),
        };

        let n = s.projection.len();
        let multi = n > 1;
        // Items normally align after the keyword. A long keyword (any DISTINCT
        // variant) would push them far to the right, so instead it sits alone
        // on its line and the items drop to the standard SELECT grid — keeping
        // them aligned with the FROM / WHERE / GROUP BY below.
        let base_grid = align_after(ci + "SELECT".len() - 1);
        let kw_grid = align_after(ci + kw.chars().count() - 1);
        let wrap = kw_grid > base_grid;
        let grid = if wrap { base_grid } else { kw_grid };
        let comma_col = grid;
        let content_col = if multi { grid + 1 } else { grid };
        let river = grid; // close-paren column for block items

        // AS-alignment columns across every aliased item (inline and block).
        // AS snaps to the tabstop past the widest value, and the alias name to
        // the tabstop past AS.
        let alias_cols = {
            let mut max_end = 0usize;
            let mut any = false;
            for item in &s.projection {
                if let SelectItem::ExprWithAlias { expr, .. } = item {
                    any = true;
                    max_end = max_end.max(self.value_end(expr, content_col));
                }
            }
            any.then(|| {
                let as_col = align_after(max_end);
                AliasCols {
                    as_col,
                    alias_col: align_after(as_col + 1),
                }
            })
        };

        let mut out = Vec::new();
        if wrap {
            // The keyword heads its own line; any header comment stays above it.
            if let Some(first) = s.projection.first() {
                self.flush_before(first.span().start.line, ci, &mut out);
            }
            out.push(format!("{}{}", pad(ci), kw));
        }
        for (i, item) in s.projection.iter().enumerate() {
            // A comment before the first item is a header at the clause indent;
            // comments between items align with the leading commas.
            let comment_col = if i == 0 { ci } else { comma_col };
            self.flush_before(item.span().start.line, comment_col, &mut out);
            let mut line = if i == 0 {
                if wrap {
                    // Keyword already emitted; the first item drops below it.
                    pad(content_col)
                } else {
                    let mut l = format!("{}{}", pad(ci), kw);
                    pad_to(&mut l, content_col);
                    l
                }
            } else {
                let mut l = pad(comma_col);
                l.push(','); // now at content_col
                l
            };
            self.render_select_item(item, &mut line, &mut out, content_col, river, alias_cols);
            self.attach_trailing(item.span().end.line, &mut out);
        }
        out
    }

    fn render_select_item(
        &self,
        item: &SelectItem,
        line: &mut String,
        out: &mut Vec<String>,
        content_col: usize,
        river: usize,
        alias_cols: Option<AliasCols>,
    ) {
        let (expr, alias_name) = match item {
            SelectItem::UnnamedExpr(e) => (Some(e), None),
            SelectItem::ExprWithAlias { expr, alias } => (Some(expr), Some(alias.to_string())),
            SelectItem::Wildcard(_) => {
                line.push('*');
                out.push(std::mem::take(line));
                return;
            }
            SelectItem::QualifiedWildcard(kind, _) => {
                line.push_str(&self.post(&kind.to_string()));
                line.push_str(".*");
                out.push(std::mem::take(line));
                return;
            }
            other => {
                line.push_str(&self.post(&other.to_string()));
                out.push(std::mem::take(line));
                return;
            }
        };
        let expr = expr.unwrap();
        let alias = Alias {
            name: alias_name.as_deref(),
            cols: alias_cols,
        };

        if let Some(block) = self.block_kind(expr) {
            self.render_block_expr(block, line, out, content_col, river, alias);
            return;
        }

        // A parenthesized AND/OR expression expands into a sub-river block.
        if let Some(inner) = nested_bool(expr) {
            line.push('(');
            out.push(std::mem::take(line));
            out.extend(self.river_lines(inner, river + 4));
            out.push(self.close_paren(content_col, alias));
            return;
        }

        // Inline item.
        line.push_str(&self.inline(expr));
        self.append_alias(line, alias);
        out.push(std::mem::take(line));
    }

    /// Append `AS <alias>` to `line`, with `AS` and the alias name snapped to
    /// the aligned columns (a single space when the alias isn't aligned).
    fn append_alias(&self, line: &mut String, alias: Alias) {
        let Some(name) = alias.name else { return };
        match alias.cols {
            Some(c) => pad_to(line, c.as_col),
            None => line.push(' '),
        }
        line.push_str("AS");
        match alias.cols {
            Some(c) => pad_to(line, c.alias_col),
            None => line.push(' '),
        }
        line.push_str(name);
    }

    /// The column of the last character of an aliased item's value, used to
    /// place the shared `AS` column. Blocks end at their closing `)` (or `END`).
    fn value_end(&self, expr: &Expr, content_col: usize) -> usize {
        if matches!(expr, Expr::Case { .. }) {
            content_col + 2 // "END"
        } else if self.is_block(expr) {
            content_col // closing ")"
        } else {
            content_col + self.inline(expr).chars().count().saturating_sub(1)
        }
    }

    // ---------- block expressions (subquery / EXISTS / window) ----------

    fn is_block(&self, e: &Expr) -> bool {
        self.block_kind(e).is_some() || nested_bool(e).is_some()
    }

    fn block_kind<'e>(&self, e: &'e Expr) -> Option<Block<'e>> {
        match e {
            Expr::Subquery(q) => Some(Block::Subquery(q)),
            Expr::Exists { subquery, negated } => Some(Block::Exists(subquery, *negated)),
            Expr::Function(f) => {
                if let Some(WindowType::WindowSpec(spec)) = &f.over {
                    if !spec.partition_by.is_empty() || !spec.order_by.is_empty() {
                        return Some(Block::Window(f));
                    }
                }
                None
            }
            Expr::Case { .. } => Some(Block::Case(e)),
            _ => None,
        }
    }

    fn render_block_expr(
        &self,
        block: Block,
        line: &mut String,
        out: &mut Vec<String>,
        content_col: usize,
        river: usize,
        alias: Alias,
    ) {
        let body_ci = river + 4;
        // The closing paren / END lines up with the opening value column, and
        // carries the aligned alias.
        match block {
            Block::Case(e) => self.render_case(e, line, out, content_col, alias),
            Block::Subquery(q) => {
                line.push('(');
                out.push(std::mem::take(line));
                out.extend(self.query(q, body_ci));
                out.push(self.close_paren(content_col, alias));
            }
            Block::Exists(q, negated) => {
                line.push_str(if negated { "NOT EXISTS (" } else { "EXISTS (" });
                out.push(std::mem::take(line));
                out.extend(self.query(q, body_ci));
                out.push(self.close_paren(content_col, alias));
            }
            Block::Window(f) => {
                // Render the call head without its OVER clause.
                let mut bare = f.clone();
                bare.over = None;
                line.push_str(&self.inline(&Expr::Function(bare)));
                line.push_str(" OVER (");
                out.push(std::mem::take(line));
                if let Some(WindowType::WindowSpec(spec)) = &f.over {
                    out.extend(self.window_body(spec, body_ci));
                }
                out.push(self.close_paren(content_col, alias));
            }
        }
    }

    /// `CASE` expands to a block: `WHEN`/`ELSE` indent +4 from the value
    /// column, `END` returns to the value column and carries the alias.
    fn render_case(
        &self,
        e: &Expr,
        line: &mut String,
        out: &mut Vec<String>,
        content_col: usize,
        alias: Alias,
    ) {
        let Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } = e
        else {
            return;
        };
        line.push_str("CASE");
        if let Some(op) = operand {
            line.push(' ');
            line.push_str(&self.inline(op));
        }
        out.push(std::mem::take(line));

        // WHEN/ELSE indent one tabstop in from the value column's tabstop
        // (the value column may itself be tabstop+1 because of the leading
        // comma); END stays at the value column, lining up with CASE.
        let when_col = floor4(content_col) + 4;
        for cw in conditions {
            let mut l = pad(when_col);
            l.push_str("WHEN ");
            l.push_str(&self.inline(&cw.condition));
            l.push_str(" THEN ");
            l.push_str(&self.inline(&cw.result));
            out.push(l);
        }
        if let Some(er) = else_result {
            let mut l = pad(when_col);
            l.push_str("ELSE ");
            l.push_str(&self.inline(er));
            out.push(l);
        }

        let mut end = pad(content_col);
        end.push_str("END");
        self.append_alias(&mut end, alias);
        out.push(end);
    }

    fn close_paren(&self, col: usize, alias: Alias) -> String {
        let mut l = pad(col);
        l.push(')');
        self.append_alias(&mut l, alias);
        l
    }

    fn window_body(&self, spec: &sqlparser::ast::WindowSpec, ci: usize) -> Vec<String> {
        // PARTITION BY and ORDER BY share one alignment group.
        let mut max_kw = 0usize;
        if !spec.partition_by.is_empty() {
            max_kw = max_kw.max("PARTITION BY".len());
        }
        if !spec.order_by.is_empty() {
            max_kw = max_kw.max("ORDER BY".len());
        }
        let grid = align_after(ci + max_kw - 1);

        let mut out = Vec::new();
        if !spec.partition_by.is_empty() {
            let items: Vec<String> = spec.partition_by.iter().map(|e| self.inline(e)).collect();
            out.extend(comma_list("PARTITION BY", &items, ci, grid));
        }
        if !spec.order_by.is_empty() {
            let items: Vec<String> = spec.order_by.iter().map(|o| self.order_item(o)).collect();
            out.extend(comma_list("ORDER BY", &items, ci, grid));
        }
        out
    }

    // ---------- FROM / JOIN ----------

    fn from(&self, tables: &[TableWithJoins], ci: usize) -> Vec<String> {
        // Collect relation descriptors first so the keyword column can be fixed
        // before rendering any derived-table body.
        let mut descs = Vec::new();
        self.push_relation_descs("FROM", tables, &mut descs);
        self.render_rows(descs, ci)
    }

    /// Render a group of relation rows (FROM/JOIN, or MERGE's INTO/USING) on the
    /// shared grid: keyword, table, alias, and `ON` each get a column; a derived
    /// table drops its `(...)` body below and carries the alias / `ON` on the
    /// closing-paren line.
    fn render_rows(&self, descs: Vec<RelationDesc>, ci: usize) -> Vec<String> {
        let (rows, cols) = self.layout_rows(descs, ci);
        let mut out = Vec::new();
        for row in &rows {
            self.emit_row(row, &cols, ci, &mut out);
        }
        out
    }

    /// Build the relation rows and resolve the shared grid columns (table,
    /// alias, `ON`, condition) for a relation group, without emitting anything.
    /// Splitting layout from emission lets a caller (e.g. UPDATE ... FROM)
    /// interleave other clauses between rows while keeping the aliases aligned.
    fn layout_rows(&self, descs: Vec<RelationDesc>, ci: usize) -> (Vec<Row>, RowCols) {
        let kw_max = descs
            .iter()
            .map(|d| d.kw.chars().count())
            .max()
            .unwrap_or(0);
        let table_col = align_after(ci + kw_max.saturating_sub(1));

        // Build rows, rendering derived-table bodies at table_col + 4.
        let rows: Vec<Row> = descs
            .into_iter()
            .map(|d| self.relation_row(d, ci, table_col))
            .collect();

        // The alias column follows the widest "table" cell; a derived table's
        // cell is just its closing paren, so it ends at the row indent.
        let table_last = rows.iter().map(|r| r.table_end).max().unwrap_or(table_col);
        let alias_col = align_after(table_last);

        let alias_last = rows
            .iter()
            .map(|r| match &r.alias {
                Some(a) => alias_col + a.chars().count().saturating_sub(1),
                None => r.table_end,
            })
            .max()
            .unwrap_or(alias_col);
        let on_col = align_after(alias_last);
        let cond_col = align_after(on_col + 1); // "ON" is 2 chars

        (
            rows,
            RowCols {
                table_col,
                alias_col,
                on_col,
                cond_col,
            },
        )
    }

    /// Emit a single relation row on the shared grid resolved by `layout_rows`.
    fn emit_row(&self, row: &Row, cols: &RowCols, ci: usize, out: &mut Vec<String>) {
        self.flush_before(row.start_line, ci, out);

        // Emit everything up to and including the line that carries the
        // alias (the table name, or a derived table's closing paren).
        let mut last = match &row.body {
            None => {
                let mut l = format!("{}{}", pad(ci), row.kw);
                pad_to(&mut l, cols.table_col);
                l.push_str(&row.table);
                l
            }
            Some(body) => {
                // Derived table: the keyword sits alone, the parenthesized
                // body drops to the next line at this indent (not the table
                // column), and the closing paren carries the alias / ON.
                out.push(format!("{}{}", pad(ci), row.kw));
                out.push(format!("{}(", pad(ci)));
                out.extend(body.iter().cloned());
                format!("{})", pad(ci))
            }
        };

        if let Some(alias) = &row.alias {
            pad_to(&mut last, cols.alias_col);
            last.push_str(alias);
        }

        match &row.on {
            None => out.push(last),
            Some(OnClause::Using(s)) => {
                pad_to(&mut last, cols.on_col);
                last.push_str(s);
                out.push(last);
            }
            Some(OnClause::On(expr)) => {
                let (ops, operands) = bool_chain(expr);
                pad_to(&mut last, cols.on_col);
                last.push_str("ON");
                pad_to(&mut last, cols.cond_col);
                last.push_str(&self.inline(operands[0]));
                out.push(last);
                for (op, operand) in ops.iter().zip(operands.iter().skip(1)) {
                    let mut l = pad(cols.on_col);
                    l.push_str(op);
                    pad_to(&mut l, cols.cond_col);
                    l.push_str(&self.inline(operand));
                    out.push(l);
                }
            }
        }
        self.attach_trailing(row.end_line, out);
    }

    fn relation_row(&self, desc: RelationDesc, ci: usize, table_col: usize) -> Row {
        let RelationDesc {
            kw,
            factor,
            on,
            start_line,
            end_line,
        } = desc;
        let (table, alias, body, table_end) = match factor {
            TableFactor::Table { name, alias, .. } => {
                let t = self.post(&name.to_string());
                let end = table_col + t.chars().count().saturating_sub(1);
                (t, alias.as_ref().map(|a| a.name.to_string()), None, end)
            }
            TableFactor::Derived {
                subquery, alias, ..
            } => {
                // Body indents one level past this clause; its closing paren
                // returns to this indent, so the table cell ends at `ci`.
                let body = self.query(subquery, ci + 4);
                (
                    String::new(),
                    alias.as_ref().map(|a| a.name.to_string()),
                    Some(body),
                    ci,
                )
            }
            // Anything exotic falls back to a compact inline table cell.
            other => {
                let t = self.post(&other.to_string());
                let end = table_col + t.chars().count().saturating_sub(1);
                (t, None, None, end)
            }
        };
        Row {
            kw,
            table,
            alias,
            on,
            start_line,
            end_line,
            body,
            table_end,
        }
    }

    fn join_parts(&self, join: &Join) -> (String, Option<OnClause>) {
        let (kw, constraint) = match &join.join_operator {
            JoinOperator::Join(c) | JoinOperator::Inner(c) => ("JOIN", Some(c)),
            JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => ("LEFT JOIN", Some(c)),
            JoinOperator::Right(c) | JoinOperator::RightOuter(c) => ("RIGHT JOIN", Some(c)),
            JoinOperator::FullOuter(c) => ("FULL JOIN", Some(c)),
            JoinOperator::CrossJoin(c) => ("CROSS JOIN", Some(c)),
            JoinOperator::LeftSemi(c) => ("LEFT SEMI JOIN", Some(c)),
            JoinOperator::RightSemi(c) => ("RIGHT SEMI JOIN", Some(c)),
            JoinOperator::LeftAnti(c) => ("LEFT ANTI JOIN", Some(c)),
            JoinOperator::RightAnti(c) => ("RIGHT ANTI JOIN", Some(c)),
            JoinOperator::Semi(c) => ("SEMI JOIN", Some(c)),
            JoinOperator::Anti(c) => ("ANTI JOIN", Some(c)),
            _ => ("JOIN", None),
        };
        let on = match constraint {
            Some(JoinConstraint::On(e)) => Some(OnClause::On(Box::new(e.clone()))),
            Some(JoinConstraint::Using(cols)) => {
                let list = cols.iter().map(|c| c.to_string()).collect::<Vec<_>>();
                Some(OnClause::Using(format!("USING ({})", list.join(", "))))
            }
            _ => None,
        };
        (kw.to_string(), on)
    }

    // ---------- WHERE / HAVING river ----------

    fn river(&self, kw: &str, expr: &Expr, ci: usize) -> Vec<String> {
        let river_col = align_after(ci + kw.len() - 1);
        let (ops, operands) = bool_chain(expr);

        let mut out = Vec::new();

        // First operand shares the keyword line.
        self.flush_before(operands[0].span().start.line, ci, &mut out);
        let mut first = format!("{}{}", pad(ci), kw);
        pad_to(&mut first, river_col);
        self.emit_operand(operands[0], first, river_col, river_col, &mut out);
        self.attach_trailing(operands[0].span().end.line, &mut out);

        for (op, operand) in ops.iter().zip(operands.iter().skip(1)) {
            self.flush_before(operand.span().start.line, river_col, &mut out);
            let mut prefix = pad(river_col);
            prefix.push_str(op);
            // Land the operand on the next tabstop so AND and OR line up.
            pad_to(&mut prefix, river_col + 4);
            self.emit_operand(operand, prefix, river_col + 4, river_col, &mut out);
            self.attach_trailing(operand.span().end.line, &mut out);
        }
        out
    }

    /// Emit one river operand onto `prefix` (already positioned so the operand
    /// starts at `operand_col`). A parenthesized AND/OR operand expands into a
    /// nested sub-river: `(` opens here, the inner chain rivers at `operand_col`,
    /// and `)` closes back at `close_col`.
    fn emit_operand(
        &self,
        operand: &Expr,
        mut prefix: String,
        operand_col: usize,
        close_col: usize,
        out: &mut Vec<String>,
    ) {
        if let Some(inner) = nested_bool(operand) {
            prefix.push('(');
            out.push(prefix);
            out.extend(self.river_lines(inner, operand_col));
            out.push(format!("{})", pad(close_col)));
        } else if let Some(block) = self.block_kind(operand) {
            // EXISTS / subquery operand expands as a block at its own column.
            self.render_block_expr(
                block,
                &mut prefix,
                out,
                operand_col,
                operand_col,
                Alias::NONE,
            );
        } else {
            prefix.push_str(&self.inline(operand));
            out.push(prefix);
        }
    }

    /// Render a boolean chain as river lines starting at `river_col` (no leading
    /// keyword). Used for the body of a parenthesized sub-river.
    fn river_lines(&self, expr: &Expr, river_col: usize) -> Vec<String> {
        let (ops, operands) = bool_chain(expr);
        let mut out = Vec::new();
        self.emit_operand(operands[0], pad(river_col), river_col, river_col, &mut out);
        for (op, operand) in ops.iter().zip(operands.iter().skip(1)) {
            let mut prefix = pad(river_col);
            prefix.push_str(op);
            pad_to(&mut prefix, river_col + 4);
            self.emit_operand(operand, prefix, river_col + 4, river_col, &mut out);
        }
        out
    }

    // ---------- GROUP BY / ORDER BY ----------

    fn group_by(&self, exprs: &[Expr], ci: usize) -> Vec<String> {
        let items: Vec<String> = exprs.iter().map(|e| self.inline(e)).collect();
        self.ordinal_list("GROUP BY", &items, ci)
    }

    fn order_by(&self, ob: &OrderBy, ci: usize) -> Vec<String> {
        let exprs = match &ob.kind {
            OrderByKind::Expressions(v) => v,
            OrderByKind::All(_) => {
                return vec![format!("{}{}", pad(ci), self.post(&ob.to_string()))]
            }
        };
        let items: Vec<String> = exprs.iter().map(|o| self.order_item(o)).collect();
        // Ordinal check looks only at the expression, not the direction.
        let all_ordinals = exprs.iter().all(|o| is_ordinal(&self.inline(&o.expr)));
        self.kw_list("ORDER BY", &items, ci, all_ordinals)
    }

    /// GROUP BY / ORDER BY: inline when every item is a bare column ordinal,
    /// otherwise one item per line with leading commas.
    fn ordinal_list(&self, kw: &str, items: &[String], ci: usize) -> Vec<String> {
        let all_ordinals = items.iter().all(|s| is_ordinal(s));
        self.kw_list(kw, items, ci, all_ordinals)
    }

    fn kw_list(&self, kw: &str, items: &[String], ci: usize, inline: bool) -> Vec<String> {
        if inline {
            return vec![format!("{}{} {}", pad(ci), kw, items.join(", "))];
        }
        // Multi-line: the comma sits tight after the keyword, content one space
        // past it (unlike the grid-aligned SELECT-list / window variant).
        let comma_col = ci + kw.len();
        comma_rows(kw, items, ci, comma_col, comma_col + 1)
    }

    fn order_item(&self, o: &OrderByExpr) -> String {
        let mut s = self.inline(&o.expr);
        match o.options.asc {
            Some(true) => s.push_str(" ASC"),
            Some(false) => s.push_str(" DESC"),
            None => {}
        }
        match o.options.nulls_first {
            Some(true) => s.push_str(" NULLS FIRST"),
            Some(false) => s.push_str(" NULLS LAST"),
            None => {}
        }
        s
    }

    fn limit(&self, lc: &sqlparser::ast::LimitClause, ci: usize) -> Vec<String> {
        use sqlparser::ast::LimitClause;
        // The value aligns to the clause grid, lining up with SELECT/FROM/WHERE.
        let grid = align_after(ci + "LIMIT".len() - 1);
        let mut out = Vec::new();
        match lc {
            LimitClause::LimitOffset {
                limit,
                offset,
                limit_by: _,
            } => {
                if let Some(l) = limit {
                    let mut line = format!("{}LIMIT", pad(ci));
                    pad_to(&mut line, grid);
                    line.push_str(&self.inline(l));
                    out.push(line);
                }
                if let Some(o) = offset {
                    // `Offset`'s Display already includes the OFFSET keyword.
                    out.push(format!("{}{}", pad(ci), self.post(&o.to_string())));
                }
            }
            LimitClause::OffsetCommaLimit { offset, limit } => {
                let mut line = format!("{}LIMIT", pad(ci));
                pad_to(&mut line, grid);
                line.push_str(&format!("{}, {}", self.inline(offset), self.inline(limit)));
                out.push(line);
            }
        }
        out
    }

    // ---------- MERGE ----------

    fn merge(&self, m: &Merge, ci: usize) -> Vec<String> {
        // `MERGE` heads its own line; the target (INTO) and source (USING) form
        // one aligned relation group, just like a FROM with one join.
        let mut out = vec![format!("{}MERGE", pad(ci))];

        let into_kw = if m.into { "INTO" } else { "" }.to_string();
        let ts = m.table.span();
        let ss = m.source.span();
        let descs = vec![
            RelationDesc {
                kw: into_kw,
                factor: &m.table,
                on: None,
                start_line: ts.start.line,
                end_line: ts.end.line,
            },
            RelationDesc {
                kw: "USING".to_string(),
                factor: &m.source,
                on: Some(OnClause::On(m.on.clone())),
                start_line: ss.start.line,
                end_line: ss.end.line,
            },
        ];
        out.extend(self.render_rows(descs, ci));

        for clause in &m.clauses {
            out.extend(self.merge_clause(clause, ci));
        }
        out
    }

    fn merge_clause(&self, clause: &MergeClause, ci: usize) -> Vec<String> {
        let kind = match clause.clause_kind {
            MergeClauseKind::Matched => "MATCHED",
            MergeClauseKind::NotMatched => "NOT MATCHED",
            MergeClauseKind::NotMatchedByTarget => "NOT MATCHED BY TARGET",
            MergeClauseKind::NotMatchedBySource => "NOT MATCHED BY SOURCE",
        };
        let mut head = format!("{}WHEN {}", pad(ci), kind);
        if let Some(pred) = &clause.predicate {
            head.push_str(" AND ");
            head.push_str(&self.inline(pred));
        }
        head.push_str(" THEN");

        // The action keyword sits one tabstop below WHEN; its body one more.
        let action = ci + 4;
        let body = ci + 8;
        let mut out = vec![head];

        match &clause.action {
            MergeAction::Delete { .. } => {
                out.push(format!("{}DELETE", pad(action)));
            }
            MergeAction::Update(u) => {
                out.push(format!("{}UPDATE SET", pad(action)));
                self.assignment_list(&u.assignments, "", 0, body, &mut out);
            }
            MergeAction::Insert(ins) => {
                let cols: Vec<String> = ins
                    .columns
                    .iter()
                    .map(|c| self.post(&c.to_string()))
                    .collect();
                match &ins.kind {
                    MergeInsertKind::Row if cols.is_empty() => {
                        out.push(format!("{}INSERT ROW", pad(action)));
                    }
                    _ => {
                        out.push(format!("{}INSERT", pad(action)));
                        self.leading_comma_block(&cols, body, &mut out);
                        match &ins.kind {
                            MergeInsertKind::Row => out.push(format!("{}ROW", pad(action))),
                            MergeInsertKind::Values(v) => {
                                self.values_rows(v, action, body, &mut out)
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn assignment_target(&self, t: &AssignmentTarget) -> String {
        match t {
            AssignmentTarget::ColumnName(name) => self.post(&name.to_string()),
            AssignmentTarget::Tuple(names) => {
                let list: Vec<String> = names.iter().map(|n| self.post(&n.to_string())).collect();
                format!("({})", list.join(", "))
            }
        }
    }

    /// A `SET` list: one `target = value` per line at `indent`, leading commas,
    /// with the `=` aligned to the tabstop past the widest target. A non-empty
    /// `lead` keyword (e.g. `SET`) prefixes the first line at `kw_ci`.
    fn assignment_list(
        &self,
        assignments: &[Assignment],
        lead: &str,
        kw_ci: usize,
        indent: usize,
        out: &mut Vec<String>,
    ) {
        let content = indent + 1;
        let targets: Vec<String> = assignments
            .iter()
            .map(|a| self.assignment_target(&a.target))
            .collect();
        let max_last = targets
            .iter()
            .map(|t| content + t.chars().count().saturating_sub(1))
            .max()
            .unwrap_or(content);
        let eq_col = align_after(max_last);
        for (i, a) in assignments.iter().enumerate() {
            let mut l = if i == 0 {
                if lead.is_empty() {
                    pad(content)
                } else {
                    let mut s = pad(kw_ci);
                    s.push_str(lead);
                    pad_to(&mut s, content);
                    s
                }
            } else {
                let mut s = pad(indent);
                s.push(',');
                s
            };
            l.push_str(&targets[i]);
            pad_to(&mut l, eq_col);
            l.push_str("= ");
            l.push_str(&self.inline(&a.value));
            out.push(l);
        }
    }

    // ---------- INSERT / UPDATE / DELETE ----------

    /// Render a `VALUES` keyword at `kw_ci` followed by each row as a hugging
    /// leading-comma block at `body`. Shared by INSERT and MERGE's INSERT
    /// action, which lay out value rows identically.
    fn values_rows(&self, v: &Values, kw_ci: usize, body: usize, out: &mut Vec<String>) {
        out.push(format!("{}VALUES", pad(kw_ci)));
        for row in &v.rows {
            let vals: Vec<String> = row.content.iter().map(|e| self.inline(e)).collect();
            self.leading_comma_block(&vals, body, out);
        }
    }

    fn insert(&self, ins: &Insert, ci: usize) -> Vec<String> {
        let mut out = Vec::new();
        let kw = if ins.replace_into {
            "REPLACE"
        } else {
            "INSERT"
        };
        let into = if ins.into { " INTO" } else { "" };
        let table = match &ins.table {
            TableObject::TableName(name) => self.post(&name.to_string()),
            other => self.post(&other.to_string()),
        };
        let mut head = format!("{}{}{} {}", pad(ci), kw, into, table);
        if let Some(alias) = &ins.table_alias {
            head.push(' ');
            head.push_str(&alias.alias.to_string());
        }
        out.push(head);

        let body = ci + 4;
        if !ins.columns.is_empty() {
            let cols: Vec<String> = ins
                .columns
                .iter()
                .map(|c| self.post(&c.to_string()))
                .collect();
            self.leading_comma_block(&cols, body, &mut out);
        }

        if let Some(src) = &ins.source {
            if let SetExpr::Values(v) = src.body.as_ref() {
                self.values_rows(v, ci, body, &mut out);
            } else {
                // INSERT ... SELECT
                out.extend(self.query(src, ci));
            }
        }
        out
    }

    fn update(&self, u: &Update, ci: usize) -> Vec<String> {
        use sqlparser::ast::UpdateTableFromKind;

        // The target table and any `FROM` tables form one aligned relation
        // group (like MERGE's INTO/USING), so their aliases share a column.
        // `SET` is then threaded between the target row and the `FROM` rows.
        let (from_tables, before_set) = match &u.from {
            Some(UpdateTableFromKind::BeforeSet(t)) => (t.as_slice(), true),
            Some(UpdateTableFromKind::AfterSet(t)) => (t.as_slice(), false),
            None => ([].as_slice(), false),
        };

        let mut descs = Vec::new();
        self.push_relation_descs("UPDATE", std::slice::from_ref(&u.table), &mut descs);
        self.push_relation_descs("FROM", from_tables, &mut descs);

        let (rows, cols) = self.layout_rows(descs, ci);

        let mut out = Vec::new();
        self.emit_row(&rows[0], &cols, ci, &mut out);
        let from_rows = &rows[1..];
        // A `FROM` before `SET` (Snowflake) precedes the assignments; the
        // standard placement is after.
        if before_set {
            for row in from_rows {
                self.emit_row(row, &cols, ci, &mut out);
            }
        }
        self.assignment_list(&u.assignments, "SET", ci, ci + 8, &mut out);
        if !before_set {
            for row in from_rows {
                self.emit_row(row, &cols, ci, &mut out);
            }
        }
        if let Some(sel) = &u.selection {
            out.extend(self.river("WHERE", sel, ci));
        }
        out
    }

    /// Append relation-row descriptors for a `&[TableWithJoins]` to `descs`,
    /// using `lead` as the first row's keyword (subsequent base tables use `,`,
    /// joins their join keyword).
    fn push_relation_descs<'a>(
        &self,
        lead: &str,
        tables: &'a [TableWithJoins],
        descs: &mut Vec<RelationDesc<'a>>,
    ) {
        for (ti, twj) in tables.iter().enumerate() {
            let kw = if ti == 0 { lead } else { "," }.to_string();
            let s = twj.relation.span();
            descs.push(RelationDesc {
                kw,
                factor: &twj.relation,
                on: None,
                start_line: s.start.line,
                end_line: s.end.line,
            });
            for join in &twj.joins {
                let (kw, on) = self.join_parts(join);
                let s = join.span();
                descs.push(RelationDesc {
                    kw,
                    factor: &join.relation,
                    on,
                    start_line: s.start.line,
                    end_line: s.end.line,
                });
            }
        }
    }

    fn delete(&self, d: &Delete, ci: usize) -> Vec<String> {
        let mut out = Vec::new();
        // `DELETE` on its own line, then the target laid out like a `FROM`.
        out.push(format!("{}DELETE", pad(ci)));
        let tables = match &d.from {
            FromTable::WithFromKeyword(t) | FromTable::WithoutKeyword(t) => t,
        };
        if !tables.is_empty() {
            out.extend(self.from(tables, ci));
        }
        if let Some(sel) = &d.selection {
            out.extend(self.river("WHERE", sel, ci));
        }
        out
    }

    /// A parenthesized leading-comma list with hugging parens: `(` glued to the
    /// first item, `)` glued to the last, leading commas under the `(` at `ci`.
    fn leading_comma_block(&self, items: &[String], ci: usize, out: &mut Vec<String>) {
        if items.is_empty() {
            out.push(format!("{}()", pad(ci)));
            return;
        }
        let last = items.len() - 1;
        for (i, it) in items.iter().enumerate() {
            let mut l = pad(ci);
            l.push(if i == 0 { '(' } else { ',' });
            l.push_str(it);
            if i == last {
                l.push(')');
            }
            out.push(l);
        }
    }

    // ---------- inline rendering helpers ----------

    fn inline(&self, e: &Expr) -> String {
        self.post(&e.to_string())
    }

    /// Restore placeholders and upper-case standard function names.
    fn post(&self, s: &str) -> String {
        let restored = self.restore_placeholders(s);
        normalize_tokens(&restored)
    }

    fn restore_placeholders(&self, s: &str) -> String {
        if self.placeholders.is_empty() {
            return s.to_string();
        }
        let mut out = s.to_string();
        // Replace in descending index order so `$sqlfmtph1` is not a prefix
        // collision with `$sqlfmtph10`.
        for (i, original) in self.placeholders.iter().enumerate().rev() {
            out = out.replace(&format!("$sqlfmtph{i}"), original);
        }
        out
    }
}

// ---------- standalone helpers ----------

enum Block<'e> {
    Subquery(&'e Query),
    Exists(&'e Query, bool),
    Window(&'e sqlparser::ast::Function),
    Case(&'e Expr),
}

/// A trailing `AS <name>` alias to render, with the columns it snaps to. A
/// `None` `name` renders nothing; `None` `cols` renders with single spaces
/// instead of aligning.
#[derive(Clone, Copy)]
struct Alias<'a> {
    name: Option<&'a str>,
    cols: Option<AliasCols>,
}

impl Alias<'_> {
    /// No alias at all (used where an expression can't carry one).
    const NONE: Alias<'static> = Alias {
        name: None,
        cols: None,
    };
}

/// The `AS` / alias-name columns a projection's aliases align to: `AS` snaps to
/// `as_col`, the name to `alias_col`.
#[derive(Clone, Copy)]
struct AliasCols {
    as_col: usize,
    alias_col: usize,
}

/// A single relation row to lay out — a FROM/JOIN entry, MERGE's INTO/USING, or
/// UPDATE's target/FROM table: its leading keyword, the table factor, an
/// optional join condition, and the source line span used for comment
/// anchoring.
struct RelationDesc<'a> {
    kw: String,
    factor: &'a TableFactor,
    on: Option<OnClause>,
    start_line: u64,
    end_line: u64,
}

/// The shared grid columns resolved for a relation group.
struct RowCols {
    table_col: usize,
    alias_col: usize,
    on_col: usize,
    cond_col: usize,
}

struct Row {
    kw: String,
    table: String,
    alias: Option<String>,
    on: Option<OnClause>,
    start_line: u64,
    end_line: u64,
    /// Pre-rendered subquery body for a derived table (`None` for plain).
    body: Option<Vec<String>>,
    /// Column index of the last char of this row's table cell.
    table_end: usize,
}

enum OnClause {
    On(Box<Expr>),
    Using(String),
}

/// Round up to the next multiple of 4.
fn ceil4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// Round down to the nearest multiple of 4 (the enclosing tabstop).
fn floor4(n: usize) -> usize {
    n / 4 * 4
}

/// Given the column index of the last character of the widest token in the
/// preceding field, return the column where the next field starts.
fn align_after(last_col: usize) -> usize {
    ceil4(last_col + 2)
}

fn pad(n: usize) -> String {
    " ".repeat(n)
}

fn pad_to(s: &mut String, col: usize) {
    let cur = s.chars().count();
    if cur < col {
        s.push_str(&" ".repeat(col - cur));
    }
}

/// True if the rendered item is a bare integer (a column ordinal).
fn is_ordinal(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Emit a leading-comma list: `kw` heads the first line with its first item at
/// `content_col`, and every later item sits under a leading comma at
/// `comma_col`. The two columns are the only thing that varies between the
/// grid-aligned variant ([`comma_list`], SELECT list) and the keyword-tight
/// variant ([`Fmt::kw_list`], GROUP BY / ORDER BY).
fn comma_rows(
    kw: &str,
    items: &[String],
    ci: usize,
    comma_col: usize,
    content_col: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i == 0 {
            let mut l = format!("{}{}", pad(ci), kw);
            pad_to(&mut l, content_col);
            l.push_str(item);
            out.push(l);
        } else {
            let mut l = pad(comma_col);
            l.push(',');
            l.push_str(item);
            out.push(l);
        }
    }
    out
}

/// A leading-comma list with the keyword and items sharing a `grid` column;
/// multiple items push the content one tabstop past the comma.
fn comma_list(kw: &str, items: &[String], ci: usize, grid: usize) -> Vec<String> {
    let content_col = if items.len() > 1 { grid + 1 } else { grid };
    comma_rows(kw, items, ci, grid, content_col)
}

/// Flatten the top-level AND/OR chain. Returns (operators, operands) where
/// operands.len() == operators.len() + 1.
fn bool_chain(e: &Expr) -> (Vec<&'static str>, Vec<&Expr>) {
    let target = match e {
        Expr::BinaryOp { op, .. } if matches!(op, BinaryOperator::And | BinaryOperator::Or) => {
            Some(op.clone())
        }
        _ => None,
    };
    let mut operands = Vec::new();
    match &target {
        Some(t) => {
            collect_chain(e, t, &mut operands);
            let label = if matches!(t, BinaryOperator::And) {
                "AND"
            } else {
                "OR"
            };
            let ops = vec![label; operands.len().saturating_sub(1)];
            (ops, operands)
        }
        None => (vec![], vec![e]),
    }
}

/// If `e` is a parenthesized AND/OR expression, return the inner chain so it
/// can be expanded into a nested sub-river.
fn nested_bool(e: &Expr) -> Option<&Expr> {
    if let Expr::Nested(inner) = e {
        if let Expr::BinaryOp { op, .. } = inner.as_ref() {
            if matches!(op, BinaryOperator::And | BinaryOperator::Or) {
                return Some(inner);
            }
        }
    }
    None
}

fn collect_chain<'e>(e: &'e Expr, target: &BinaryOperator, out: &mut Vec<&'e Expr>) {
    if let Expr::BinaryOp { left, op, right } = e {
        if op == target {
            collect_chain(left, target, out);
            collect_chain(right, target, out);
            return;
        }
    }
    out.push(e);
}

fn set_op_str(op: &SetOperator, q: &SetQuantifier) -> String {
    let base = match op {
        SetOperator::Union => "UNION",
        SetOperator::Except => "EXCEPT",
        SetOperator::Intersect => "INTERSECT",
        SetOperator::Minus => "MINUS",
    };
    match q {
        SetQuantifier::All => format!("{base} ALL"),
        SetQuantifier::Distinct => format!("{base} DISTINCT"),
        SetQuantifier::ByName => format!("{base} BY NAME"),
        SetQuantifier::AllByName => format!("{base} ALL BY NAME"),
        SetQuantifier::DistinctByName => format!("{base} DISTINCT BY NAME"),
        SetQuantifier::None => base.to_string(),
    }
}

/// Restore the original spelling of each not-equal operator and voice
/// displeasure at `<>`.
///
/// sqlparser collapses both `!=` and `<>` to `<>`, so `neqs` carries the
/// source spelling of each not-equal in order (true = `!=`, false = `<>`).
/// Operators are matched against the `<>` tokens in the rendered output (which
/// appear in the same order); a surviving `<>` gets a trailing `-- 🤮`.
pub fn voice_not_equal(text: &str, neqs: &[bool]) -> String {
    let emoji = "🤮";
    let mut idx = 0;
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        // Skip comment-only lines (their contents are not SQL).
        if trimmed.starts_with("--") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            lines.push(line.to_string());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut res = String::with_capacity(line.len());
        let mut in_string = false;
        let mut gross = false;
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            if in_string {
                res.push(c);
                if c == '\'' {
                    if chars.get(j + 1) == Some(&'\'') {
                        res.push('\'');
                        j += 2;
                        continue;
                    }
                    in_string = false;
                }
                j += 1;
                continue;
            }
            if c == '\'' {
                in_string = true;
                res.push(c);
                j += 1;
                continue;
            }
            // A trailing line comment ends the SQL portion of the line.
            if c == '-' && chars.get(j + 1) == Some(&'-') {
                res.extend(chars[j..].iter());
                break;
            }
            if c == '<' && chars.get(j + 1) == Some(&'>') {
                match neqs.get(idx).copied() {
                    Some(true) => res.push_str("!="),
                    _ => {
                        res.push_str("<>");
                        gross = true;
                    }
                }
                idx += 1;
                j += 2;
                continue;
            }
            res.push(c);
            j += 1;
        }
        // Voice displeasure, unless this line already carries the emoji (e.g.
        // on a re-format, where it survives as a real trailing comment).
        if gross && !res.contains(emoji) {
            res.push_str("  -- ");
            res.push_str(emoji);
        }
        lines.push(res);
    }
    lines.join("\n")
}

/// Token-level normalization of a rendered expression, skipping string
/// literals: upper-case standard function names (an identifier immediately
/// followed by `(`) and bare TRUE/FALSE/NULL.
fn normalize_tokens(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\'' {
                // Doubled quote escapes within a string literal.
                if i + 1 < chars.len() && chars[i + 1] == '\'' {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '\'' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let preceded_by_dot = start > 0 && chars[start - 1] == '.';
            let followed_by_paren = i < chars.len() && chars[i] == '(';
            let is_func = followed_by_paren && funcs::is_standard(&word);
            let is_bare_kw = !followed_by_paren
                && matches!(
                    word.to_ascii_lowercase().as_str(),
                    "true" | "false" | "null"
                );
            if !preceded_by_dot && (is_func || is_bare_kw) {
                out.push_str(&word.to_ascii_uppercase());
            } else {
                out.push_str(&word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}
