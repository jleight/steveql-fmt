CREATE TABLE IF NOT EXISTS reporting.daily_totals
AS
SELECT   day            AS  day
        ,SUM(amount)    AS  total
FROM    sales
GROUP BY day;

CREATE OR REPLACE TEMPORARY TABLE staged
AS
SELECT  *
FROM    staging.raw_events
WHERE   loaded_at > NOW() - INTERVAL '1 day';

CREATE VIEW reporting.active_customers
AS
SELECT   c.id
        ,c.name
FROM    customers   c
WHERE   EXISTS (
            SELECT  1
            FROM    orders  o
            WHERE   o.customer_id = c.id
        );

CREATE MATERIALIZED VIEW reporting.revenue_by_region
AS
SELECT   region         AS  region
        ,SUM(amount)    AS  revenue
FROM    sales
GROUP BY region
