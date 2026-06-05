MERGE
INTO    warehouse.inventory t
USING
(
    SELECT   sku
            ,SUM(qty)   AS  qty
    FROM    staging.receipts
    GROUP BY sku
)                           s   ON  t.sku = s.sku
WHEN MATCHED THEN
    UPDATE SET
         qty        = t.qty + s.qty
        ,updated_at = NOW()
WHEN NOT MATCHED THEN
    INSERT
        (sku
        ,qty
        ,updated_at)
    VALUES
        (s.sku
        ,s.qty
        ,NOW())
