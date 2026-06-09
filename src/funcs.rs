//! Curated list of "standard" SQL functions that get upper-cased on output.
//!
//! Anything NOT in this list keeps the casing it was written with, so
//! Postgres-style functions such as `pg_table_size`, `to_tsvector`, or your
//! own `my_helper()` stay lower-case.
//!
//! Edit `STANDARD_FUNCTIONS` to taste — names are matched case-insensitively.

/// Names are stored lower-case; matching is case-insensitive.
pub const STANDARD_FUNCTIONS: &[&str] = &[
    // Aggregates
    "array_agg",
    "avg",
    "bit_and",
    "bit_or",
    "bool_and",
    "bool_or",
    "coalesce",
    "corr",
    "count",
    "covar_pop",
    "covar_samp",
    "every",
    "greatest",
    "json_agg",
    "json_object_agg",
    "jsonb_agg",
    "least",
    "max",
    "min",
    "mode",
    "nullif",
    "percentile_cont",
    "percentile_disc",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "string_agg",
    "sum",
    "var_pop",
    "var_samp",
    "variance",
    // Window / ranking
    "cume_dist",
    "dense_rank",
    "first_value",
    "lag",
    "last_value",
    "lead",
    "nth_value",
    "ntile",
    "percent_rank",
    "rank",
    "row_number",
    // Common scalar standard functions
    "abs",
    "age",
    "btrim",
    "cardinality",
    "ceil",
    "ceiling",
    "char_length",
    "character_length",
    "concat",
    "concat_ws",
    "date_part",
    "date_trunc",
    "exp",
    "extract",
    "floor",
    "initcap",
    "left",
    "length",
    "ln",
    "log",
    "lower",
    "lpad",
    "ltrim",
    "mod",
    "now",
    "overlay",
    "position",
    "power",
    "repeat",
    "replace",
    "reverse",
    "right",
    "round",
    "rpad",
    "rtrim",
    "sign",
    "split_part",
    "sqrt",
    "substr",
    "substring",
    "trim",
    "trunc",
    "upper",
];

/// True if `name` (any casing) is a standard function that should be
/// upper-cased.
pub fn is_standard(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    STANDARD_FUNCTIONS.contains(&lower.as_str())
}
