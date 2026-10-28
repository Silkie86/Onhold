-- Reverts sending from workspaces' own domains.

DROP FUNCTION sender_domain_taken(text);
DROP TABLE sender_domains;
