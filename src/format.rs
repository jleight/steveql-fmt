//! The opinionated block formatter.
//!
//! sqlparser handles parsing; this module decides layout. Inline expressions
//! are rendered with sqlparser's `Display` (then post-processed for
//! placeholders and standard-function casing), while the clause structure
//! (SELECT list, FROM/JOIN, WHERE river, CTEs, subqueries, window specs) is
//! laid out by hand on a 4-column alignment grid.

use crate::funcs;
use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, ConflictTarget, Cte, Delete, Distinct, Expr,
    FromTable, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, Insert, Join,
    JoinConstraint, JoinOperator, Merge, MergeAction, MergeClause, MergeClauseKind,
    MergeInsertKind, MergeUpdateKind, NamedWindowDefinition, NamedWindowExpr, OnConflictAction,
    OnInsert, OrderBy, OrderByExpr, OrderByKind, OutputClause, Query, Select, SelectItem, SetExpr,
    SetOperator, SetQuantifier, Spanned, Statement, TableAlias, TableFactor, TableObject,
    TableSampleKind, TableWithJoins, Update, Values, WindowType,
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
            Statement::CreateTable(ct) if ct.query.is_some() => {
                out.extend(self.query_after_header(stmt, ct.query.as_ref().unwrap()))
            }
            Statement::CreateView(cv) => out.extend(self.query_after_header(stmt, &cv.query)),
            // Anything we don't lay out by hand falls back to compact SQL.
            other => out.push(self.post(&other.to_string())),
        }
        // Any comments after the last anchored element (e.g. a trailing footer)
        // are emitted so nothing is ever dropped.
        self.flush_before(u64::MAX, 0, &mut out);
        out.join("\n")
    }

    /// A statement that ends in a query (`CREATE TABLE ... AS`, `CREATE VIEW
    /// ... AS`): everything before `AS` stays on one header line, `AS` gets a
    /// line of its own, and the query is laid out below it at the statement's
    /// indent. Anything after the query (e.g. `WITH NO DATA`) can't be split
    /// off that way, so such a statement stays compact.
    fn query_after_header(&self, stmt: &Statement, q: &Query) -> Vec<String> {
        let full = stmt.to_string();
        let body = q.to_string();
        match full.strip_suffix(&body) {
            Some(header) => {
                let header = header.trim_end();
                let mut out = match header.strip_suffix(" AS") {
                    Some(name) => vec![self.post(name), "AS".to_string()],
                    None => vec![self.post(header)],
                };
                out.extend(self.query(q, 0));
                out
            }
            None => vec![self.post(&full)],
        }
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

        // `FETCH` and the locking clauses read naturally as one line each.
        if let Some(fetch) = &q.fetch {
            out.push(format!("{}{}", pad(ci), self.post(&fetch.to_string())));
        }
        for lock in &q.locks {
            out.push(format!("{}{}", pad(ci), self.post(&lock.to_string())));
        }

        out
    }

    fn render_with(&self, recursive: bool, ctes: &[Cte], ci: usize, out: &mut Vec<String>) {
        let name_indent = ci + 4;
        let body_ci = ci + 8;
        for (i, cte) in ctes.iter().enumerate() {
            let name = alias_text(&cte.alias);
            let as_kw = match &cte.materialized {
                Some(m) => format!("AS {m}"),
                None => "AS".to_string(),
            };
            self.flush_before(cte.query.span().start.line, name_indent, out);
            if i == 0 {
                let kw = if recursive { "WITH RECURSIVE" } else { "WITH" };
                out.push(format!("{}{} {} {} (", pad(ci), kw, name, as_kw));
            } else {
                out.push(format!("{},{} {} (", pad(name_indent), name, as_kw));
            }
            out.extend(self.query(&cte.query, body_ci));
            out.push(format!("{})", pad(name_indent)));
        }
    }

    fn set_expr(&self, body: &SetExpr, ci: usize) -> Vec<String> {
        match body {
            SetExpr::Select(s) => self.select(s, ci),
            // A parenthesized query keeps its parentheses, which decide what an
            // ORDER BY or LIMIT inside a set operation applies to. Laid out like
            // a derived table: `(` alone, the body one level in, `)` back out.
            SetExpr::Query(q) => {
                let mut out = vec![format!("{}(", pad(ci))];
                out.extend(self.query(q, ci + 4));
                out.push(format!("{})", pad(ci)));
                out
            }
            SetExpr::SetOperation {
                left,
                op,
                set_quantifier,
                right,
            } => {
                // A blank line sets each query apart from the operator, unless
                // the query is parenthesized: its parens already do that.
                let mut out = self.set_expr(left, ci);
                if !ends_in_parens(left) {
                    out.push(String::new());
                }
                out.push(format!("{}{}", pad(ci), set_op_str(op, set_quantifier)));
                if !starts_in_parens(right) {
                    out.push(String::new());
                }
                out.extend(self.set_expr(right, ci));
                out
            }
            SetExpr::Values(v) => {
                let mut out = Vec::new();
                self.values_rows(v, ci, ci + 4, &mut out);
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
        if let Some(into) = &s.into {
            // `SelectInto`'s Display leads with the INTO keyword itself.
            let target = into.to_string();
            let target = target.strip_prefix("INTO ").unwrap_or(&target);
            out.push(clause_line("INTO", &self.post(target), ci));
        }
        if !s.from.is_empty() {
            out.extend(self.from(&s.from, ci));
        }
        if let Some(sel) = &s.selection {
            out.extend(self.river("WHERE", sel, ci));
        }
        match &s.group_by {
            GroupByExpr::All(_) => out.push(format!("{}GROUP BY ALL", pad(ci))),
            GroupByExpr::Expressions(exprs, modifiers) => {
                if !exprs.is_empty() {
                    out.extend(self.group_by(exprs, ci));
                }
                if let Some(last) = out.last_mut().filter(|_| !modifiers.is_empty()) {
                    for m in modifiers {
                        last.push(' ');
                        last.push_str(&m.to_string());
                    }
                }
            }
        }
        if let Some(having) = &s.having {
            out.extend(self.river("HAVING", having, ci));
        }
        // WINDOW and QUALIFY keep the order they were written in.
        if s.window_before_qualify {
            out.extend(self.named_windows(&s.named_window, ci));
        }
        if let Some(qualify) = &s.qualify {
            out.extend(self.river("QUALIFY", qualify, ci));
        }
        if !s.window_before_qualify {
            out.extend(self.named_windows(&s.named_window, ci));
        }
        out
    }

    /// The `WINDOW` clause: one definition per line with leading commas, each
    /// spec expanded like an `OVER (...)` block in the SELECT list.
    fn named_windows(&self, defs: &[NamedWindowDefinition], ci: usize) -> Vec<String> {
        let grid = align_after(ci + "WINDOW".len() - 1);
        let content_col = if defs.len() > 1 { grid + 1 } else { grid };
        let mut out = Vec::new();
        for (i, NamedWindowDefinition(name, expr)) in defs.iter().enumerate() {
            let mut line = if i == 0 {
                let mut l = format!("{}WINDOW", pad(ci));
                pad_to(&mut l, content_col);
                l
            } else {
                let mut l = pad(grid);
                l.push(',');
                l
            };
            line.push_str(&format!("{name} AS "));
            match expr {
                NamedWindowExpr::WindowSpec(spec) if expands(spec) => {
                    line.push('(');
                    out.push(line);
                    out.extend(self.window_body(spec, grid + 4));
                    out.push(format!("{})", pad(content_col)));
                }
                other => {
                    line.push_str(&self.post(&other.to_string()));
                    out.push(line);
                }
            }
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
            Some(Distinct::Distinct) => "SELECT DISTINCT".to_string(),
            Some(Distinct::All) => "SELECT ALL".to_string(),
        };
        self.item_list(&kw, &s.projection, ci)
    }

    /// A SELECT list under `kw`. Items normally align after the keyword. A
    /// long keyword (any DISTINCT variant) would push them far to the right,
    /// so instead it sits alone on its line and the items drop to the standard
    /// SELECT grid — keeping them aligned with the FROM / WHERE / GROUP BY
    /// below.
    fn item_list(&self, kw: &str, items: &[SelectItem], ci: usize) -> Vec<String> {
        let base_grid = align_after(ci + "SELECT".len() - 1);
        let kw_grid = align_after(ci + kw.chars().count() - 1);
        let wrap = kw_grid > base_grid;
        let grid = if wrap { base_grid } else { kw_grid };
        self.items_on_grid(kw, items, ci, grid, wrap)
    }

    /// A `RETURNING` list: the keyword alone on its line, the items one
    /// tabstop in, like an INSERT column list.
    fn returning(&self, items: &[SelectItem], ci: usize) -> Vec<String> {
        self.items_on_grid("RETURNING", items, ci, ci + 4, true)
    }

    /// A list of SELECT items with leading commas at `grid` (content one past
    /// it when there are several) and aligned aliases. With `wrap`, `kw` heads
    /// its own line; otherwise the first item follows it.
    fn items_on_grid(
        &self,
        kw: &str,
        items: &[SelectItem],
        ci: usize,
        grid: usize,
        wrap: bool,
    ) -> Vec<String> {
        let multi = items.len() > 1;
        let comma_col = grid;
        let content_col = if multi { grid + 1 } else { grid };
        let river = grid; // close-paren column for block items

        // AS-alignment columns across every aliased item (inline and block).
        // AS snaps to the tabstop past the widest value, and the alias name to
        // the tabstop past AS.
        let alias_cols = {
            let mut max_end = 0usize;
            let mut any = false;
            for item in items {
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
            if let Some(first) = items.first() {
                self.flush_before(first.span().start.line, ci, &mut out);
            }
            out.push(format!("{}{}", pad(ci), kw));
        }
        for (i, item) in items.iter().enumerate() {
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

        if let Some(split) = self.split(expr) {
            self.render_split(split, line, out, content_col, river, alias);
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
    /// place the shared `AS` column. A block ends on its closing line: `)` or
    /// `END` plus whatever follows it, like `END)` or `) = 1`.
    fn value_end(&self, expr: &Expr, content_col: usize) -> usize {
        let width = match self.split(expr) {
            Some(s) => s.block.close().len() + s.tail.chars().count(),
            None if nested_bool(expr).is_some() => 1, // closing ")"
            None => self.inline(expr).chars().count(),
        };
        content_col + width.saturating_sub(1)
    }

    // ---------- block expressions (subquery / window / CASE) ----------

    /// Find the block an expression lays out around: a subquery, an expanding
    /// window spec, or a `CASE`, either the expression itself or one level
    /// down (an operand of a comparison, the target of `IN`, an argument of
    /// a call, the query of `EXISTS`). Everything around the block renders
    /// inline, before its opening line or after its closing one. A short
    /// subquery isn't a block; it stays inline.
    fn split<'e>(&self, e: &'e Expr) -> Option<Split<'e>> {
        let wrap = |head: String, inner: Split<'e>, tail: &str| inner.wrap(head, tail);
        match e {
            Expr::Subquery(q) if !is_short(q) => Some(Split::bare(Block::Query(q))),
            Expr::Exists { subquery, negated } if !is_short(subquery) => {
                let kw = if *negated { "NOT EXISTS " } else { "EXISTS " };
                Some(wrap(
                    kw.to_string(),
                    Split::bare(Block::Query(subquery)),
                    "",
                ))
            }
            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } if !is_short(subquery) => {
                let not = if *negated { "NOT " } else { "" };
                let head = format!("{} {not}IN ", self.inline(expr));
                Some(wrap(head, Split::bare(Block::Query(subquery)), ""))
            }
            Expr::Case { .. } => Some(Split::bare(Block::Case(e))),
            Expr::Function(f) => {
                if let Some(WindowType::WindowSpec(spec)) = &f.over {
                    if !expands(spec) {
                        return None;
                    }
                    let mut bare = f.clone();
                    bare.over = None;
                    let head = format!("{} OVER ", self.inline(&Expr::Function(bare)));
                    return Some(wrap(head, Split::bare(Block::Window(spec)), ""));
                }
                // A lone argument holding a block wraps it (`SUM(CASE`). With
                // several, the call becomes the block: one argument per line.
                let open = self.post(&format!("{}(", f.name));
                match plain_args(f)? {
                    [arg] => Some(wrap(open, self.split(unnamed_expr(arg)?)?, ")")),
                    args => {
                        let any_block = args
                            .iter()
                            .any(|a| unnamed_expr(a).is_some_and(|e| self.split(e).is_some()));
                        any_block.then(|| wrap(open, Split::bare(Block::Args(args)), ""))
                    }
                }
            }
            Expr::BinaryOp { left, op, right }
                if !matches!(op, BinaryOperator::And | BinaryOperator::Or) =>
            {
                if let Some(s) = self.split(left) {
                    let tail = format!(" {op} {}", self.inline(right));
                    Some(wrap(String::new(), s, &tail))
                } else {
                    let head = format!("{} {op} ", self.inline(left));
                    Some(wrap(head, self.split(right)?, ""))
                }
            }
            _ => None,
        }
    }

    /// Split a parenthesized list (a VALUES row) at its first item holding a
    /// block: `open` and the items before it lead the
    /// opening line, and the items after it follow the block's closing `)` /
    /// `END`, along with the list's own `)`. `exprs` has the expression behind
    /// each of `texts`, where there is one to split.
    fn split_list<'e>(
        &self,
        open: &str,
        texts: &[String],
        exprs: &[Option<&'e Expr>],
    ) -> Option<Split<'e>> {
        let (i, inner) = exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| Some((i, self.split((*e)?)?)))?;
        let mut head = open.to_string();
        for t in &texts[..i] {
            head.push_str(t);
            head.push_str(", ");
        }
        let mut tail = String::new();
        for t in &texts[i + 1..] {
            tail.push_str(", ");
            tail.push_str(t);
        }
        tail.push(')');
        Some(inner.wrap(head, &tail))
    }

    /// An expression laid out after `prefix`: one line when it's inline, or,
    /// when it holds a block, the opening line followed by the block's body
    /// and closing line (already at their columns). The first line isn't
    /// indented, so the caller can place it.
    fn value_lines(
        &self,
        prefix: String,
        e: &Expr,
        content_col: usize,
        river: usize,
    ) -> Vec<String> {
        let mut line = prefix;
        match self.split(e) {
            Some(split) => {
                let mut lines = Vec::new();
                self.render_split(
                    split,
                    &mut line,
                    &mut lines,
                    content_col,
                    river,
                    Alias::NONE,
                );
                lines
            }
            None => {
                line.push_str(&self.inline(e));
                vec![line]
            }
        }
    }

    /// Lay out a split expression onto `line`, which is already positioned at
    /// `content_col`: the head and the block's opening on this line, the
    /// block's body one tabstop past `river`, and the closing `)` / `END` back
    /// at `content_col`, carrying the tail and the aligned alias.
    fn render_split(
        &self,
        split: Split,
        line: &mut String,
        out: &mut Vec<String>,
        content_col: usize,
        river: usize,
        alias: Alias,
    ) {
        let body_ci = river + 4;
        line.push_str(&split.head);
        match split.block {
            Block::Query(q) => {
                line.push('(');
                out.push(std::mem::take(line));
                out.extend(self.query(q, body_ci));
            }
            Block::Args(args) => {
                // One argument per line, a tabstop in, with leading commas.
                out.push(std::mem::take(line));
                let comma_col = body_ci;
                let content = comma_col + 1;
                let items: Vec<Vec<String>> = args
                    .iter()
                    .map(|a| match unnamed_expr(a) {
                        Some(e) => self.value_lines(String::new(), e, content, comma_col),
                        None => vec![self.post(&a.to_string())],
                    })
                    .collect();
                out.extend(comma_block_rows(
                    "", &items, comma_col, comma_col, content, "",
                ));
            }
            Block::Window(spec) => {
                line.push('(');
                out.push(std::mem::take(line));
                out.extend(self.window_body(spec, body_ci));
            }
            Block::Case(e) => {
                out.push(std::mem::take(line));
                self.case_body(e, body_ci, out);
            }
        }
        let mut close = pad(content_col);
        close.push_str(split.block.close());
        close.push_str(&split.tail);
        self.append_alias(&mut close, alias);
        out.push(close);
    }

    /// A `CASE` block up to its `END`: `CASE` (and any operand) is appended to
    /// the opening line already in `out`, then one `WHEN` / `ELSE` per line at
    /// `when_col`.
    fn case_body(&self, e: &Expr, when_col: usize, out: &mut Vec<String>) {
        let Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } = e
        else {
            return;
        };
        let open = out.last_mut().expect("opening line");
        open.push_str("CASE");
        if let Some(op) = operand {
            open.push(' ');
            open.push_str(&self.inline(op));
        }
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
    }

    fn close_paren(&self, col: usize, alias: Alias) -> String {
        let mut l = pad(col);
        l.push(')');
        self.append_alias(&mut l, alias);
        l
    }

    fn window_body(&self, spec: &sqlparser::ast::WindowSpec, ci: usize) -> Vec<String> {
        // A frame splits into its units (ROWS / RANGE / GROUPS), which join the
        // keyword column, and the extent, which sits on the grid.
        let frame = spec.window_frame.as_ref().map(|f| {
            // A frame has no Display of its own; a spec holding only the frame
            // renders as just the frame.
            let only_frame = sqlparser::ast::WindowSpec {
                window_name: None,
                partition_by: vec![],
                order_by: vec![],
                window_frame: Some(f.clone()),
            };
            let text = self.post(&only_frame.to_string());
            match text.split_once(' ') {
                Some((units, extent)) => (units.to_string(), extent.to_string()),
                None => (text, String::new()),
            }
        });

        // PARTITION BY, ORDER BY, and the frame units share one alignment group.
        let mut max_kw = 0usize;
        if !spec.partition_by.is_empty() {
            max_kw = max_kw.max("PARTITION BY".len());
        }
        if !spec.order_by.is_empty() {
            max_kw = max_kw.max("ORDER BY".len());
        }
        if let Some((units, _)) = &frame {
            max_kw = max_kw.max(units.len());
        }
        let grid = align_after(ci + max_kw.max(1) - 1);

        let mut out = Vec::new();
        // A window built on a named one names it first, on its own line.
        if let Some(name) = &spec.window_name {
            out.push(format!("{}{}", pad(ci), name));
        }
        if !spec.partition_by.is_empty() {
            let items: Vec<String> = spec.partition_by.iter().map(|e| self.inline(e)).collect();
            out.extend(comma_list("PARTITION BY", &items, ci, grid));
        }
        if !spec.order_by.is_empty() {
            let items: Vec<String> = spec.order_by.iter().map(|o| self.order_item(o)).collect();
            out.extend(comma_list("ORDER BY", &items, ci, grid));
        }
        if let Some((units, extent)) = frame {
            out.push(clause_line_at(&units, &extent, ci, grid));
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
        // Comma-separated tables lead with a comma like a SELECT list, so the
        // table cells sit one past the grid to leave the comma its column.
        let comma = descs.iter().any(|d| d.kw == ",");
        let comma_col = align_after(ci + kw_max.saturating_sub(1));
        let table_col = if comma { comma_col + 1 } else { comma_col };

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
                comma_col,
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
                let mut l = if row.kw == "," {
                    format!("{},", pad(cols.comma_col))
                } else {
                    format!("{}{}", pad(ci), row.kw)
                };
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
            TableFactor::Table { alias, .. } => {
                // The cell is everything but the alias: arguments of a table
                // function, WITH ORDINALITY, a TABLESAMPLE written before an
                // alias. Any other TABLESAMPLE moves to the alias cell.
                let mut bare = factor.clone();
                let mut moved = None;
                if let TableFactor::Table {
                    alias: a,
                    sample: s,
                    ..
                } = &mut bare
                {
                    if a.is_none() || matches!(s, Some(TableSampleKind::AfterTableAlias(_))) {
                        moved = s.take();
                    }
                    *a = None;
                }
                let t = self.post(&bare.to_string());
                let end = table_col + t.chars().count().saturating_sub(1);
                let sample = moved.map(|s| self.post(&sample_text(&s)));
                (t, alias_cell(alias.as_ref(), sample), None, end)
            }
            TableFactor::Derived {
                subquery,
                alias,
                sample,
                ..
            } => {
                // Body indents one level past this clause; its closing paren
                // returns to this indent, so the table cell ends at `ci`.
                let body = self.query(subquery, ci + 4);
                let sample = sample.as_ref().map(|s| self.post(&sample_text(s)));
                (
                    String::new(),
                    alias_cell(alias.as_ref(), sample),
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
        let kw = match constraint {
            Some(JoinConstraint::Natural) => format!("NATURAL {kw}"),
            _ => kw.to_string(),
        };
        let on = match constraint {
            Some(JoinConstraint::On(e)) => Some(OnClause::On(Box::new(e.clone()))),
            Some(JoinConstraint::Using(cols)) => {
                let list = cols.iter().map(|c| c.to_string()).collect::<Vec<_>>();
                Some(OnClause::Using(format!("USING ({})", list.join(", "))))
            }
            _ => None,
        };
        (kw, on)
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
        } else if let Some(split) = self.split(operand) {
            // A subquery / window / CASE operand expands as a block at its own
            // column.
            self.render_split(
                split,
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
        comma_rows(kw, items, ci, comma_col, comma_col + 1, "")
    }

    fn order_item(&self, o: &OrderByExpr) -> String {
        // The options render with their own leading spaces (` DESC NULLS LAST`).
        let mut s = self.inline(&o.expr);
        s.push_str(&self.post(&o.options.to_string()));
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
                    // `Offset`'s Display leads with the OFFSET keyword itself.
                    let text = self.post(&o.to_string());
                    let value = text.strip_prefix("OFFSET ").unwrap_or(&text);
                    out.push(clause_line_at("OFFSET", value, ci, grid));
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
        match &m.output {
            Some(OutputClause::Returning { select_items, .. }) => {
                out.extend(self.returning(select_items, ci))
            }
            Some(other) => out.push(format!("{}{}", pad(ci), self.post(&other.to_string()))),
            None => {}
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
            MergeAction::DoNothing { .. } => {
                out.push(format!("{}DO NOTHING", pad(action)));
            }
            MergeAction::Update(u) => {
                match &u.kind {
                    MergeUpdateKind::Set(assignments) => {
                        out.push(format!("{}UPDATE SET", pad(action)));
                        self.assignment_list(assignments, "", 0, body, &mut out);
                    }
                    MergeUpdateKind::Wildcard => out.push(format!("{}UPDATE SET *", pad(action))),
                }
                // Oracle's `WHERE` / `DELETE WHERE` follow the assignments.
                if let Some(pred) = &u.update_predicate {
                    out.extend(self.river("WHERE", pred, action));
                }
                if let Some(pred) = &u.delete_predicate {
                    out.extend(self.river("DELETE WHERE", pred, action));
                }
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
                    MergeInsertKind::Wildcard => {
                        out.push(format!("{}INSERT *", pad(action)));
                    }
                    _ => {
                        out.push(format!("{}INSERT", pad(action)));
                        self.leading_comma_block(&cols, body, &mut out);
                        match &ins.kind {
                            MergeInsertKind::Values(v) => {
                                self.values_rows(v, action, body, &mut out)
                            }
                            _ => out.push(format!("{}ROW", pad(action))),
                        }
                    }
                }
                // Oracle's `WHERE` follows the inserted values.
                if let Some(pred) = &ins.insert_predicate {
                    out.extend(self.river("WHERE", pred, action));
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
    /// with the `=` aligned to the tabstop past the widest target. A lone
    /// assignment needs neither, so it sits at `indent` with a single space
    /// before `=`. A non-empty `lead` keyword (e.g. `SET`) prefixes the first
    /// line at `kw_ci`.
    fn assignment_list(
        &self,
        assignments: &[Assignment],
        lead: &str,
        kw_ci: usize,
        indent: usize,
        out: &mut Vec<String>,
    ) {
        let multi = assignments.len() > 1;
        let content = if multi { indent + 1 } else { indent };
        let targets: Vec<String> = assignments
            .iter()
            .map(|a| self.assignment_target(&a.target))
            .collect();
        let eq_col = if multi {
            let max_last = targets
                .iter()
                .map(|t| content + t.chars().count().saturating_sub(1))
                .max()
                .unwrap_or(content);
            align_after(max_last)
        } else {
            content + targets[0].chars().count() + 1
        };
        // Each item is a complete `target = value`, with `=` snapped to `eq_col`
        // relative to where the item lands (`content`); the leading-comma /
        // keyword skeleton is then shared with every other clause. A value
        // holding a block runs onto the lines below, closing at `content`.
        let items: Vec<Vec<String>> = assignments
            .iter()
            .zip(&targets)
            .map(|(a, t)| {
                let mut s = t.clone();
                pad_to(&mut s, eq_col - content);
                s.push_str("= ");
                self.value_lines(s, &a.value, content, indent)
            })
            .collect();
        out.extend(comma_block_rows(lead, &items, kw_ci, indent, content, ""));
    }

    // ---------- INSERT / UPDATE / DELETE ----------

    /// Render a `VALUES` keyword at `kw_ci` followed by its rows at `body`.
    /// A single row is a hugging leading-comma block, one value per line.
    /// Several rows put each row on one line, in a leading-comma list. Either
    /// way, a value holding a block expands it, closing at the value's column.
    /// Shared by INSERT, MERGE's INSERT action, and a VALUES query.
    fn values_rows(&self, v: &Values, kw_ci: usize, body: usize, out: &mut Vec<String>) {
        out.push(format!("{}VALUES", pad(kw_ci)));
        let content = body + 1;
        match v.rows.as_slice() {
            [row] => {
                let vals: Vec<Vec<String>> = row
                    .content
                    .iter()
                    .map(|e| self.value_lines(String::new(), e, content, body))
                    .collect();
                out.extend(comma_block_rows("(", &vals, body, body, content, ")"));
            }
            rows => {
                let rows: Vec<Vec<String>> = rows
                    .iter()
                    .map(|row| {
                        let texts: Vec<String> =
                            row.content.iter().map(|e| self.inline(e)).collect();
                        let exprs: Vec<Option<&Expr>> = row.content.iter().map(Some).collect();
                        match self.split_list("(", &texts, &exprs) {
                            Some(split) => {
                                let mut lines = Vec::new();
                                let mut line = String::new();
                                self.render_split(
                                    split,
                                    &mut line,
                                    &mut lines,
                                    content,
                                    body,
                                    Alias::NONE,
                                );
                                lines
                            }
                            None => vec![format!("({})", texts.join(", "))],
                        }
                    })
                    .collect();
                out.extend(comma_block_rows("", &rows, body, body, content, ""));
            }
        }
    }

    fn insert(&self, ins: &Insert, ci: usize) -> Vec<String> {
        let mut out = Vec::new();
        let kw = if ins.replace_into {
            "REPLACE"
        } else {
            "INSERT"
        };
        let mut kw = kw.to_string();
        if let Some(or) = &ins.or {
            kw.push(' ');
            kw.push_str(&or.to_string());
        }
        if ins.ignore {
            kw.push_str(" IGNORE");
        }
        let into = if ins.into { " INTO" } else { "" };
        let table = match &ins.table {
            TableObject::TableName(name) => self.post(&name.to_string()),
            other => self.post(&other.to_string()),
        };
        let mut head = format!("{}{}{} {}", pad(ci), kw, into, table);
        // PostgreSQL requires the AS here, unlike a FROM alias.
        if let Some(alias) = &ins.table_alias {
            head.push_str(" AS ");
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

        match &ins.source {
            Some(src) => {
                if let SetExpr::Values(v) = src.body.as_ref() {
                    self.values_rows(v, ci, body, &mut out);
                } else {
                    // INSERT ... SELECT
                    out.extend(self.query(src, ci));
                }
            }
            None if !ins.assignments.is_empty() => {
                self.assignment_list(&ins.assignments, "SET", ci, ci + 8, &mut out);
            }
            None => out.push(format!("{}DEFAULT VALUES", pad(ci))),
        }

        match &ins.on {
            Some(OnInsert::OnConflict(oc)) => {
                let mut head = format!("{}ON CONFLICT", pad(ci));
                match &oc.conflict_target {
                    Some(ConflictTarget::Columns(cols)) => {
                        let list: Vec<String> = cols.iter().map(|c| c.to_string()).collect();
                        head.push_str(&format!(" ({})", list.join(", ")));
                    }
                    Some(other) => {
                        head.push(' ');
                        head.push_str(other.to_string().trim());
                    }
                    None => {}
                }
                match &oc.action {
                    OnConflictAction::DoNothing => {
                        head.push_str(" DO NOTHING");
                        out.push(head);
                    }
                    // Laid out like MERGE's WHEN ... THEN UPDATE SET: the
                    // action one tabstop in, its assignments one more.
                    OnConflictAction::DoUpdate(du) => {
                        head.push_str(" DO");
                        out.push(head);
                        out.push(format!("{}UPDATE SET", pad(ci + 4)));
                        self.assignment_list(&du.assignments, "", 0, ci + 8, &mut out);
                        if let Some(sel) = &du.selection {
                            out.extend(self.river("WHERE", sel, ci + 4));
                        }
                    }
                }
            }
            Some(other) => out.push(format!(
                "{}{}",
                pad(ci),
                self.post(other.to_string().trim())
            )),
            None => {}
        }

        if let Some(items) = &ins.returning {
            out.extend(self.returning(items, ci));
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
        if let Some(items) = &u.returning {
            out.extend(self.returning(items, ci));
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
            let kw = with_lateral(kw, &twj.relation);
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
                let kw = with_lateral(kw, &join.relation);
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
        // FROM and USING form one relation group, so their aliases align.
        let mut descs = Vec::new();
        self.push_relation_descs("FROM", tables, &mut descs);
        if let Some(using) = &d.using {
            self.push_relation_descs("USING", using, &mut descs);
        }
        if !descs.is_empty() {
            out.extend(self.render_rows(descs, ci));
        }
        if let Some(sel) = &d.selection {
            out.extend(self.river("WHERE", sel, ci));
        }
        if let Some(items) = &d.returning {
            out.extend(self.returning(items, ci));
        }
        out
    }

    /// A parenthesized leading-comma list with hugging parens: `(` glued to the
    /// first item, `)` glued to the last, leading commas under the `(` at `ci`.
    fn leading_comma_block(&self, items: &[String], ci: usize, out: &mut Vec<String>) {
        out.extend(comma_rows("(", items, ci, ci, ci + 1, ")"));
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

/// Something that lays out over several lines inside an expression.
#[derive(Clone, Copy)]
enum Block<'e> {
    Query(&'e Query),
    Window(&'e sqlparser::ast::WindowSpec),
    Case(&'e Expr),
    /// The arguments of a call, one per line.
    Args(&'e [FunctionArg]),
}

impl Block<'_> {
    /// The text that ends the block on its closing line.
    fn close(&self) -> &'static str {
        match self {
            Block::Query(_) | Block::Window(_) | Block::Args(_) => ")",
            Block::Case(_) => "END",
        }
    }
}

/// An expression around a block: the inline text before the block on its
/// opening line, and the inline text after it on its closing line.
struct Split<'e> {
    head: String,
    block: Block<'e>,
    tail: String,
}

impl<'e> Split<'e> {
    fn bare(block: Block<'e>) -> Self {
        Split {
            head: String::new(),
            block,
            tail: String::new(),
        }
    }

    /// The same block with more inline text around it.
    fn wrap(self, head: String, tail: &str) -> Self {
        Split {
            head: head + &self.head,
            block: self.block,
            tail: self.tail + tail,
        }
    }
}

/// Whether a subquery is short enough to stay inline: one item from at most
/// one table and nothing else, so expanding it would only give two lines.
fn is_short(q: &Query) -> bool {
    let SetExpr::Select(s) = q.body.as_ref() else {
        return false;
    };
    let no_group_by = match &s.group_by {
        GroupByExpr::Expressions(exprs, modifiers) => exprs.is_empty() && modifiers.is_empty(),
        GroupByExpr::All(_) => false,
    };
    q.with.is_none()
        && q.order_by.is_none()
        && q.limit_clause.is_none()
        && q.fetch.is_none()
        && q.locks.is_empty()
        && s.distinct.is_none()
        && s.into.is_none()
        && s.projection.len() == 1
        && s.from.len() <= 1
        && s.from.iter().all(|t| t.joins.is_empty())
        && s.selection.is_none()
        && no_group_by
        && s.having.is_none()
        && s.qualify.is_none()
        && s.named_window.is_empty()
}

/// The expression of a plain positional argument.
fn unnamed_expr(arg: &FunctionArg) -> Option<&Expr> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
        _ => None,
    }
}

/// The arguments of a plain call like `COALESCE(x, 0)`, or `None` for anything
/// more (DISTINCT, FILTER, OVER, an ORDER BY inside the parens).
fn plain_args(f: &sqlparser::ast::Function) -> Option<&[FunctionArg]> {
    if f.over.is_some() || f.filter.is_some() || !f.within_group.is_empty() {
        return None;
    }
    let FunctionArguments::List(list) = &f.args else {
        return None;
    };
    if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
        return None;
    }
    Some(&list.args)
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
    /// Where a comma-separated table's leading comma goes.
    comma_col: usize,
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

/// A table alias as it appears in a relation's alias column: the name, plus
/// its column list when it has one. The `AS` is left off, as everywhere else.
fn alias_text(a: &TableAlias) -> String {
    if a.columns.is_empty() {
        return a.name.to_string();
    }
    let cols: Vec<String> = a.columns.iter().map(|c| c.to_string()).collect();
    format!("{} ({})", a.name, cols.join(", "))
}

/// A `LATERAL` derived table says so as part of its keyword (`CROSS JOIN
/// LATERAL`), so the whole phrase sits in the keyword column.
fn with_lateral(kw: String, factor: &TableFactor) -> String {
    match factor {
        TableFactor::Derived { lateral: true, .. } => format!("{kw} LATERAL"),
        _ => kw,
    }
}

/// A relation's alias cell: the alias, then any TABLESAMPLE on the next
/// tabstop (or in the alias's place when there's no alias).
fn alias_cell(alias: Option<&TableAlias>, sample: Option<String>) -> Option<String> {
    let sample = sample.map(|s| s.trim().to_string());
    match (alias.map(alias_text), sample) {
        (Some(mut cell), Some(sample)) => {
            // The alias column sits on a tabstop, so this snaps to one too.
            let col = align_after(cell.chars().count() - 1);
            pad_to(&mut cell, col);
            cell.push_str(&sample);
            Some(cell)
        }
        (cell, None) => cell,
        (None, sample) => sample,
    }
}

/// A TABLESAMPLE clause, wherever it was written.
fn sample_text(kind: &TableSampleKind) -> String {
    match kind {
        TableSampleKind::BeforeTableAlias(s) | TableSampleKind::AfterTableAlias(s) => s.to_string(),
    }
}

/// Whether a window spec expands into a block rather than staying inline:
/// anything but a bare `OVER ()` or `OVER (name)`.
fn expands(spec: &sqlparser::ast::WindowSpec) -> bool {
    !spec.partition_by.is_empty() || !spec.order_by.is_empty() || spec.window_frame.is_some()
}

/// A one-line clause: `kw` at `ci`, `value` on the clause grid after it.
fn clause_line(kw: &str, value: &str, ci: usize) -> String {
    clause_line_at(kw, value, ci, align_after(ci + kw.len() - 1))
}

/// A one-line clause with `value` at an explicit `grid` column.
fn clause_line_at(kw: &str, value: &str, ci: usize, grid: usize) -> String {
    let mut line = format!("{}{}", pad(ci), kw);
    if !value.is_empty() {
        pad_to(&mut line, grid);
        line.push_str(value);
    }
    line
}

/// Round up to the next multiple of 4.
fn ceil4(n: usize) -> usize {
    n.div_ceil(4) * 4
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

/// Emit a wrapped, leading-comma item list — the one layout primitive behind
/// every comma-separated clause:
/// - the first line is `lead` (a clause keyword, or an opening `(`) followed by
///   the first item at `content_col`;
/// - each later item sits under a leading comma at `comma_col`;
/// - `close` (e.g. `)`) is appended after the last item.
///
/// The call sites differ only in these delimiters and columns: a keyword lead
/// with an empty close (GROUP BY / ORDER BY, window specs, UPDATE SET) versus a
/// `(`/`)` pair glued to the items (INSERT columns and value rows).
fn comma_rows(
    lead: &str,
    items: &[String],
    ci: usize,
    comma_col: usize,
    content_col: usize,
    close: &str,
) -> Vec<String> {
    let items: Vec<Vec<String>> = items.iter().map(|i| vec![i.clone()]).collect();
    comma_block_rows(lead, &items, ci, comma_col, content_col, close)
}

/// `comma_rows` for items that may span several lines: an item's first line
/// is placed like a one-line item, and the rest (already laid out at their
/// own columns) follow as they are.
fn comma_block_rows(
    lead: &str,
    items: &[Vec<String>],
    ci: usize,
    comma_col: usize,
    content_col: usize,
    close: &str,
) -> Vec<String> {
    if items.is_empty() {
        return vec![format!("{}{}{}", pad(ci), lead, close)];
    }
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let mut l = if i == 0 {
            let mut l = format!("{}{}", pad(ci), lead);
            pad_to(&mut l, content_col);
            l
        } else {
            let mut l = pad(comma_col);
            l.push(',');
            l
        };
        l.push_str(&item[0]);
        out.push(l);
        out.extend(item[1..].iter().cloned());
    }
    if let Some(last) = out.last_mut() {
        last.push_str(close);
    }
    out
}

/// A leading-comma list with the keyword and items sharing a `grid` column;
/// multiple items push the content one tabstop past the comma.
fn comma_list(kw: &str, items: &[String], ci: usize, grid: usize) -> Vec<String> {
    let content_col = if items.len() > 1 { grid + 1 } else { grid };
    comma_rows(kw, items, ci, grid, content_col, "")
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

/// Whether the last query of a set-operation operand is parenthesized.
fn ends_in_parens(e: &SetExpr) -> bool {
    match e {
        SetExpr::Query(_) => true,
        SetExpr::SetOperation { right, .. } => ends_in_parens(right),
        _ => false,
    }
}

/// Whether the first query of a set-operation operand is parenthesized.
fn starts_in_parens(e: &SetExpr) -> bool {
    match e {
        SetExpr::Query(_) => true,
        SetExpr::SetOperation { left, .. } => starts_in_parens(left),
        _ => false,
    }
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
