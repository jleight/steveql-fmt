SELECT   id
        ,status
        ,region
FROM    orders
WHERE   status != 'cancelled'
        AND region <> 'test'  -- 🤮
        AND amount <> 0  -- 🤮
