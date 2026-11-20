SELECT   o.id   AS  id
        ,c.name AS  customer
        ,p.name AS  product
        ,s.name AS  status
FROM        orders      o
INNER JOIN  customers   c   ON  c.id = o.customer_id
LEFT JOIN   products    p   ON  p.id = o.product_id
LEFT JOIN   statuses    s   ON  s.id = o.status_id
                            AND s.active = TRUE
WHERE   o.created_at >= '2026-01-01'
