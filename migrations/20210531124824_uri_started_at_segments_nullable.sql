-- Add migration script here
alter table recording alter column stream_uri drop not null;
alter table recording alter column started_at drop not null;
alter table recording alter column segments drop not null;
