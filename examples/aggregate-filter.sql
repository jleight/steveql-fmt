SELECT   k.account_id                                   AS  account_id
        ,COUNT(*) FILTER (WHERE k.revoked_at IS NULL)   AS  unrevoked
        ,COUNT(*) FILTER (
            WHERE   k.revoked_at IS NULL
                    AND (
                        k.expires_at IS NULL
                        OR  k.expires_at > NOW()
                    )
         )                                              AS  active
FROM    api_keys    k
GROUP BY k.account_id
