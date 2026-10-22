SELECT   id                                     AS  id
        ,sku                                    AS  sku
        ,LOWER(name)                            AS  name
        ,ROUND(price_cents / 100.0, 2)          AS  price
        ,COALESCE(category, 'uncategorized')    AS  category
FROM    products
WHERE   active = TRUE
