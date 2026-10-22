SELECT   customer_id            AS  customer_id
        ,created_at             AS  created_at
        ,amount                 AS  amount
        ,SUM(amount) OVER (
            PARTITION BY    customer_id
            ORDER BY        created_at
            ROWS            BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
         )                      AS  running_total
        ,AVG(amount) OVER (
            by_customer
            RANGE   BETWEEN INTERVAL '30 days' PRECEDING AND CURRENT ROW
         )                      AS  monthly_average
        ,RANK() OVER by_amount  AS  amount_rank
FROM    orders
WINDOW   by_customer AS (
            PARTITION BY    customer_id
            ORDER BY        created_at
         )
        ,by_amount AS (
            PARTITION BY    customer_id
            ORDER BY        amount DESC
         );

SELECT   customer_id
        ,order_id
FROM    orders
QUALIFY ROW_NUMBER() OVER (
            PARTITION BY    customer_id
            ORDER BY        created_at DESC
        ) = 1
