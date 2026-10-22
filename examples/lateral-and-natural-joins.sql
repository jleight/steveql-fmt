SELECT   c.id
        ,l.created_at
FROM                customers   c
CROSS JOIN LATERAL
(
    SELECT  o.created_at
    FROM    orders  o
    WHERE   o.customer_id = c.id
    ORDER BY o.created_at DESC
    LIMIT   1
)                               l;

SELECT   c.id               AS  id
        ,COALESCE(l.n, 0)   AS  recent_orders
FROM                customers   c
LEFT JOIN LATERAL
(
    SELECT  COUNT(*)    AS  n
    FROM    orders  o
    WHERE   o.customer_id = c.id
            AND o.created_at >= NOW() - INTERVAL '30 days'
)                               l   ON  TRUE;

SELECT   id
        ,name
        ,region
FROM            stores
NATURAL JOIN    store_regions;

SELECT   v.code
        ,v.label
FROM
(
    VALUES
         ('a', 'Alpha')
        ,('b', 'Beta')
)               v (code, label)
JOIN    codes   c (code, active)    ON  c.code = v.code
