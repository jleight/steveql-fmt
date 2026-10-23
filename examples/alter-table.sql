ALTER TABLE billing.orders
    ADD COLUMN  discount    NUMERIC(12, 2)
                            NOT NULL
                            DEFAULT 0;

ALTER TABLE IF EXISTS billing.orders
     DROP COLUMN    legacy_code CASCADE
    ,ALTER COLUMN   total       SET DATA TYPE BIGINT
    ,ALTER COLUMN   status      SET NOT NULL
    ,ALTER COLUMN   notes       DROP DEFAULT;

ALTER TABLE events.page_views
    ALTER COLUMN    viewed_at   TYPE TIMESTAMPTZ
                                USING viewed_at AT TIME ZONE 'UTC';

ALTER TABLE billing.order_lines
    ADD CONSTRAINT  order_lines_sku_fkey    FOREIGN KEY (sku)
                                            REFERENCES catalog.products (sku)
                                            ON DELETE CASCADE;

ALTER TABLE ONLY billing.order_lines
    DROP CONSTRAINT order_lines_sku_fkey;

ALTER TABLE accounts.users
    RENAME COLUMN   name    TO display_name;

ALTER TABLE staged_events
    RENAME TO   raw_events;

ALTER TABLE accounts.users
    OWNER TO    app_owner
