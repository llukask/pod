alter table user_episode add constraint unique_user_episode unique (user_id, episode_id);
