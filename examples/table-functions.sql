SELECT   r.sku
        ,r.qty
FROM    read_csv($1, header = TRUE, all_varchar = TRUE) r
WHERE   r.qty != '0';

SELECT   g.n
        ,g.i
FROM    generate_series(1, 10) WITH ORDINALITY  g (n, i);

SELECT   o.id
        ,j.key
        ,j.value
FROM     orders                     o
        ,jsonb_each(o.attributes)   j (key, value)
WHERE   j.key != 'internal';

SELECT   d.day                  AS  day
        ,COALESCE(s.total, 0)   AS  total
FROM        generate_series(DATE '2026-01-01', DATE '2026-01-31', INTERVAL '1 day') d (day)
LEFT JOIN   daily_sales                                                             s       ON  s.day = d.day
