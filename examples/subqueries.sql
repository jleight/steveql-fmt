SELECT   c.id   AS  id
        ,c.name AS  name
        ,(
            SELECT  COUNT(*)
            FROM    orders  o
            WHERE   o.customer_id = c.id
         )      AS  order_count
        ,EXISTS (
            SELECT  1
            FROM    flags   f
            WHERE   f.customer_id = c.id
                    AND f.kind = 'vip'
         )      AS  is_vip
FROM    customers   c
