SELECT ALL
        region
FROM    sales;

SELECT   id
        ,email
INTO    TEMPORARY recent_signups
FROM    accounts
WHERE   created_at >= NOW() - INTERVAL '1 day'
