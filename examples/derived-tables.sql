SELECT   t.region
        ,t.total
FROM
(
    SELECT   region         AS  region
            ,SUM(amount)    AS  total
    FROM    sales
    WHERE   amount > 0
    GROUP BY region
)   t
JOIN
(
    SELECT   region     AS  region
            ,COUNT(*)   AS  n
    FROM    stores
    GROUP BY region
)   s   ON  s.region = t.region
WHERE   t.total > 1000
ORDER BY 2 DESC
