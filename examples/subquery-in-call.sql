SELECT   c.id   AS  id
        ,COALESCE(
             (
                SELECT  SUM(o.amount)
                FROM    orders  o
                WHERE   o.customer_id = c.id
             )
            ,0
         )      AS  total
FROM    customers   c;

SELECT  c.id
FROM    customers   c
WHERE   GREATEST(
             (
                SELECT  MAX(o.created_at)
                FROM    orders  o
                WHERE   o.customer_id = c.id
             )
            ,c.updated_at
        ) > NOW() - INTERVAL '30 days'
