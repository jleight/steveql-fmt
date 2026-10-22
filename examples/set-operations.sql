(
    SELECT   id
            ,created_at
    FROM    orders
    ORDER BY created_at DESC
    LIMIT   5
)
UNION ALL
(
    SELECT   id
            ,created_at
    FROM    returns
    ORDER BY created_at DESC
    LIMIT   5
)
ORDER BY 2 DESC;

SELECT  id
FROM    customers
WHERE   region = 'east'

EXCEPT

SELECT  customer_id
FROM    blocked
