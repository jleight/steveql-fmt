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
ORDER BY 1
