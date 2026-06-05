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
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "coalesce",
    "greatest",
    "least",
    "nullif",
    "array_agg",
    "string_agg",
    "bool_and",
    "bool_or",
    "every",
    "bit_and",
    "bit_or",
    "json_agg",
    "jsonb_agg",
    "json_object_agg",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "variance",
    "var_pop",
    "var_samp",
    "corr",
    "covar_pop",
    "covar_samp",
    "percentile_cont",
    "percentile_disc",
    "mode",
    // Window / ranking
    "row_number",
    "rank",
    "dense_rank",
    "percent_rank",
    "cume_dist",
    "ntile",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "nth_value",
    // Common scalar standard functions
    "abs",
    "ceil",
    "ceiling",
    "floor",
    "round",
    "trunc",
    "mod",
    "power",
    "sqrt",
    "exp",
    "ln",
    "log",
    "sign",
    "length",
    "char_length",
    "character_length",
    "lower",
    "upper",
    "trim",
    "ltrim",
    "rtrim",
    "btrim",
    "substring",
    "substr",
    "position",
    "overlay",
    "replace",
    "concat",
    "concat_ws",
    "left",
    "right",
    "reverse",
    "repeat",
    "lpad",
    "rpad",
    "split_part",
    "initcap",
    "now",
    "extract",
    "date_part",
    "date_trunc",
    "age",
    "cardinality",
    "nullif",
];

/// True if `name` (any casing) is a standard function that should be
/// upper-cased.
pub fn is_standard(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    STANDARD_FUNCTIONS.contains(&lower.as_str())
}
