SELECT   region         AS  region
        ,channel        AS  channel
        ,COUNT(*)       AS  orders
        ,SUM(amount)    AS  revenue
FROM    sales
GROUP BY region
        ,channel
ORDER BY revenue DESC
        ,region;

SELECT   region     AS  region
        ,COUNT(*)   AS  n
FROM    sales
GROUP BY 1
ORDER BY 1, 2
