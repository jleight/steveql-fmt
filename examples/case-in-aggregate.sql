SELECT   region         AS  region
        ,SUM(CASE
            WHEN channel = 'web' THEN amount
            ELSE 0
         END)           AS  web_revenue
        ,COUNT(CASE
            WHEN refunded THEN 1
         END)           AS  refunds
        ,SUM(amount)    AS  revenue
FROM    sales
GROUP BY region
HAVING  SUM(CASE
            WHEN channel = 'store' THEN amount
            ELSE 0
        END) > 0
