SELECT   id
        ,email
FROM    users
WHERE   deleted_at IS NULL
        AND (
            status = 'active'
            OR  (
                status = 'pending'
                AND verified = TRUE
            )
        )
        AND email IS NOT NULL
        AND region IN ('us', 'ca', 'mx')
ORDER BY 1
