SELECT  a
FROM    t
WHERE   total = (
            SELECT  SUM(x)
            FROM    o
            WHERE   o.tid = t.id
        )
        AND active;

SELECT  a
FROM    t
WHERE   id IN (
            SELECT  tid
            FROM    orders
            WHERE   amount > 100
        )
        AND active;

SELECT  a
FROM    t
WHERE   id NOT IN (SELECT tid FROM orders);
