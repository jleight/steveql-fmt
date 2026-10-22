INSERT INTO t
    (a
    ,b)
VALUES
    ((SELECT MAX(id) FROM s)
    ,'x');

INSERT INTO t
    (a
    ,b)
VALUES
    ((
        SELECT  MAX(id)
        FROM    s
        WHERE   active
     )
    ,'x');

INSERT INTO t
    (a
    ,b)
VALUES
     ('x', (
        SELECT  MAX(id)
        FROM    s
        WHERE   active
     ))
    ,('y', 2);
