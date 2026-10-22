SELECT   id     AS  id
        ,amount AS  amount
        ,CASE
            WHEN amount >= 1000 THEN 'large'
            WHEN amount >= 100 THEN 'medium'
            ELSE 'small'
         END    AS  bucket
        ,CASE status
            WHEN 'active' THEN 1
            ELSE 0
         END    AS  is_active
FROM    orders
