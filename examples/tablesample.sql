SELECT   id
        ,amount
FROM    orders  TABLESAMPLE SYSTEM (5)
WHERE   amount > 100;

SELECT  o.id
FROM    orders      o   TABLESAMPLE BERNOULLI (1) REPEATABLE (42)
JOIN    customers   c                                               ON  c.id = o.customer_id
