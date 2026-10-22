SELECT  a
FROM    t
WHERE   id IN (
            SELECT  tid
            FROM    orders
            WHERE   customer_id IN (
                        SELECT  id
                        FROM    customers
                        WHERE   active
                    )
        );
