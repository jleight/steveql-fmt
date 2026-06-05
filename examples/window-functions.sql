SELECT   customer_id
        ,order_id
        ,amount
        ,ROW_NUMBER() OVER (
            PARTITION BY    customer_id
            ORDER BY        created_at DESC
         )  AS  recency_rank
        ,SUM(amount) OVER (
            PARTITION BY    customer_id
         )  AS  customer_total
FROM    orders
