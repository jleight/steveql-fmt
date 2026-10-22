INSERT INTO warehouse.inventory AS i
    (sku
    ,qty
    ,updated_at)
VALUES
    (%(sku)s
    ,%(qty)s
    ,NOW())
ON CONFLICT (sku) DO
    UPDATE SET
         qty        = i.qty + excluded.qty
        ,updated_at = NOW()
    WHERE   i.locked = FALSE;

INSERT INTO tags
    (name)
SELECT DISTINCT
        tag
FROM    staging.import_tags
ON CONFLICT DO NOTHING;

INSERT INTO accounts
    (email
    ,plan)
VALUES
    (%(email)s
    ,'free')
ON CONFLICT ON CONSTRAINT accounts_email_key DO NOTHING;

INSERT OR IGNORE INTO seen_events
    (event_id)
VALUES
    ($1)
