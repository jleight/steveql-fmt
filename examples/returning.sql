INSERT INTO events.audit_log
    (actor_id
    ,action)
VALUES
    (%(actor)s
    ,'login')
RETURNING
     id         AS  id
    ,created_at AS  logged_at;

UPDATE  accounts.subscriptions
SET      plan       = 'pro'
        ,updated_at = NOW()
WHERE   account_id = %(account)s
RETURNING
    id;

DELETE
FROM    sessions.tokens t
USING   accounts.users  u
WHERE   t.user_id = u.id
        AND u.disabled = TRUE
RETURNING
     t.id
    ,u.email;

MERGE
INTO    warehouse.inventory t
USING   staging.receipts    s   ON  t.sku = s.sku
WHEN MATCHED THEN
    UPDATE SET
        qty = t.qty + s.qty
RETURNING
     t.sku
    ,t.qty;

WITH expired AS (
        DELETE
        FROM    sessions.tokens
        WHERE   expires_at < NOW()
        RETURNING
            user_id
    )
INSERT INTO sessions.expirations
    (user_id
    ,expired_at)
SELECT   user_id
        ,NOW()
FROM    expired
