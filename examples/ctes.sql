WITH RECURSIVE descendants AS (
        SELECT   id
                ,parent_id
        FROM    categories
        WHERE   parent_id IS NULL
        UNION ALL
        SELECT   c.id
                ,c.parent_id
        FROM    categories  c
        JOIN    descendants d   ON  c.parent_id = d.id
    )
    ,counts AS (
        SELECT   parent_id
                ,COUNT(*)   AS  n
        FROM    descendants
        GROUP BY 1
    )
SELECT   d.id
        ,c.n
FROM        descendants d
LEFT JOIN   counts      c   ON  c.parent_id = d.id
ORDER BY 1;

WITH stale AS (
        SELECT  id
        FROM    products
        WHERE   last_seen_at < NOW() - INTERVAL '90 days'
        LIMIT   1000
    )
UPDATE  products    p
SET      status     = 'archived'
        ,updated_at = NOW()
FROM    stale       s
WHERE   p.id = s.id;
