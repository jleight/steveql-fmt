SELECT   id                     AS  id
        ,LOWER(email)           AS  email
        ,pg_typeof(id)          AS  kind
        ,CAST(amount AS BIGINT) AS  amt
        ,active = TRUE          AS  is_active
        ,deleted_at IS NULL     AS  alive
FROM    events
