CREATE SECRET s3_prod (
     TYPE   s3
    ,KEY_ID 'AKIAIOSFODNN7EXAMPLE'
    ,SECRET 'wJalrXUtnFEMI/K7MDENG'
    ,REGION 'us-east-1'
);

CREATE OR REPLACE SECRET gcs_default (
     TYPE       gcs
    ,PROVIDER   credential_chain
);

CREATE TEMPORARY SECRET (
     TYPE           http
    ,BEARER_TOKEN   'example-token'
    ,SCOPE          'https://api.example.com'
);

DROP SECRET s3_prod
