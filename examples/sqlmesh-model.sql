-- steveql-fmt: ignore
MODEL (
  name analytics.daily_orders,
  kind INCREMENTAL_BY_TIME_RANGE (
    time_column order_date
  ),
  grain order_date,
);

SELECT   order_date     AS  order_date
        ,COUNT(*)       AS  orders
        ,SUM(amount)    AS  revenue
FROM    orders
WHERE   order_date BETWEEN @start_ds AND @end_ds
GROUP BY 1
ORDER BY 1
