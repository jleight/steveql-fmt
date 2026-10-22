UPDATE  t
SET      total  = (
            SELECT  SUM(amount)
            FROM    orders  o
            WHERE   o.tid = t.id
         )
        ,name   = 'x'
WHERE   id = 1;
