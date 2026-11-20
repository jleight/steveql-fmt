SELECT   u.id                       AS  id
        ,COALESCE(r.n, 0)::BIGINT   AS  reported
        ,(r.n + c.n + a.n)::BIGINT  AS  actions
        ,(
            COALESCE(r.bytes, 0)
            +   COALESCE(c.bytes, 0)
            +   COALESCE(a.bytes, 0)
         )::BIGINT                  AS  bytes
FROM        principals  u
LEFT JOIN   reported    r   ON  r.account_id = u.id
LEFT JOIN   commented   c   ON  c.account_id = u.id
LEFT JOIN   attached    a   ON  a.account_id = u.id
