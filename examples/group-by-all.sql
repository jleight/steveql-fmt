SELECT   region         AS  region
        ,channel        AS  channel
        ,COUNT(*)       AS  orders
        ,SUM(amount)    AS  revenue
FROM    sales
GROUP BY ALL;

SELECT   region         AS  region
        ,channel        AS  channel
        ,SUM(amount)    AS  revenue
FROM    sales
GROUP BY ROLLUP (region, channel)
ORDER BY region
        ,channel
