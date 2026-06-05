INSERT INTO events.audit_log
    (actor_id
    ,action
    ,target_id
    ,created_at)
VALUES
    (%(actor)s
    ,'update'
    ,%(target)s
    ,NOW());

UPDATE  accounts.subscriptions
SET      plan       = 'pro'
        ,seats      = seats + 5
        ,updated_at = NOW()
WHERE   account_id = %(account)s
        AND status != 'cancelled';

DELETE
FROM    sessions.tokens
WHERE   expires_at < NOW()
        AND revoked = TRUE
