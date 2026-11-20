SELECT   id
        ,name
FROM    accounts
WHERE   tenant_id = %s
        AND region = %(region)s
        AND created_at >= %(since)s;

SELECT  p.id
FROM    principals  p
WHERE   p.id = @owner_id
        OR  p.owner_id = @owner_id
