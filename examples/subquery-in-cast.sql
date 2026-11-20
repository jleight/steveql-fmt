SELECT   c.id       AS  id
        ,(
            SELECT  COUNT(*)
            FROM    orders  o
            WHERE   o.customer_id = c.id
         )::BIGINT  AS  order_count
        ,COALESCE(
             (
                SELECT  ARRAY_AGG(t.name ORDER BY t.name)
                FROM        customer_tags   ct
                INNER JOIN  tags            t   ON  t.id = ct.tag_id
                                                AND t.archived_at IS NULL
                WHERE   ct.customer_id = c.id
             )
            ,'{}'
         )::TEXT[]  AS  tag_names
FROM    customers   c
