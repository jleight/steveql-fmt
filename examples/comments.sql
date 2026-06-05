-- Customers and their order counts.
SELECT   c.id
        ,c.email  -- contact address
        -- only counts non-cancelled orders
        ,COUNT(o.id)    AS  order_count
FROM        customers   c
LEFT JOIN   orders      o   ON  o.customer_id = c.id
                            AND o.status != 'cancelled'
GROUP BY 1, 2
