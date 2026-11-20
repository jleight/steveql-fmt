WITH recent AS MATERIALIZED (
        SELECT   id
                ,customer_id
        FROM    orders
        WHERE   created_at >= NOW() - INTERVAL '7 days'
    )
    ,totals (customer_id, n) AS NOT MATERIALIZED (
        SELECT   customer_id
                ,COUNT(*)
        FROM    recent
        GROUP BY customer_id
    )
SELECT   c.name
        ,t.n
FROM        totals      t
INNER JOIN  customers   c   ON  c.id = t.customer_id;

WITH RECURSIVE tree (id, parent_id, depth) AS (
        SELECT   id
                ,parent_id
                ,0
        FROM    categories
        WHERE   parent_id IS NULL

        UNION ALL

        SELECT   c.id
                ,c.parent_id
                ,t.depth + 1
        FROM        categories  c
        INNER JOIN  tree        t   ON  c.parent_id = t.id
    )
SELECT   id
        ,depth
FROM    tree
ORDER BY 2
