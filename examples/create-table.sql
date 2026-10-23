CREATE TABLE accounts.users (
     id         BIGINT                  GENERATED ALWAYS AS IDENTITY
                                        PRIMARY KEY
    ,email      TEXT        NOT NULL    UNIQUE
    ,name       TEXT        NULL
    ,created_at TIMESTAMPTZ NOT NULL    DEFAULT NOW()

    ,CONSTRAINT users_email_lower   CHECK (email = LOWER(email))
);

CREATE TABLE IF NOT EXISTS billing.order_lines (
     order_id   BIGINT          NOT NULL    REFERENCES billing.orders (id)
                                            ON DELETE CASCADE
    ,line_no    INTEGER         NOT NULL
    ,sku        VARCHAR(32)     NOT NULL
    ,unit_price NUMERIC(12, 2)  NOT NULL    CHECK (unit_price >= 0)
    ,qty        INTEGER         NOT NULL    DEFAULT 1

    ,PRIMARY KEY (order_id, line_no)
    ,FOREIGN KEY (sku)
     REFERENCES catalog.products (sku)
);

CREATE TEMPORARY TABLE staged_events (
     id         BIGINT
    ,payload    JSONB
);

CREATE TABLE events.page_views (
     id         BIGINT                  GENERATED ALWAYS AS IDENTITY
    ,account_id BIGINT      NOT NULL
    ,viewed_at  TIMESTAMPTZ NOT NULL

    ,PRIMARY KEY (id, viewed_at)
)
PARTITION BY RANGE (viewed_at)
