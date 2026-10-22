SELECT   id
        ,payload
FROM    jobs.queue
WHERE   state = 'ready'
ORDER BY priority DESC
        ,id
LIMIT   10
FOR UPDATE SKIP LOCKED;

SELECT  id
FROM    jobs.queue
WHERE   id = %(id)s
FOR UPDATE OF queue NOWAIT;

SELECT   id
        ,score
FROM    leaderboard
ORDER BY score DESC
OFFSET  10 ROWS
FETCH FIRST 10 ROWS WITH TIES;

SELECT  id
FROM    accounts
WHERE   tier = 'gold'
FOR SHARE
