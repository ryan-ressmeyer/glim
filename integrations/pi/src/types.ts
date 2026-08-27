export interface PublishFileInput {
  path: string;
  published_filename?: string;
  caption?: string;
  media_type?: string;
  collect_assets?: boolean;
}

export interface PublishInput {
  title: string;
  commentary: string;
  files: PublishFileInput[];
  predecessor_post_id?: number;
  open?: boolean;
}

export interface CliSuccess<T = unknown> {
  schema_version: 1;
  ok: true;
  result: T;
}

export interface BrowserLaunch {
  requested: boolean;
  opened: boolean;
  error: null | { code: string; message: string; details: Record<string, unknown> };
}

export interface PublicationDetails {
  pi_session_id: string;
  external_session_key: string;
  public_session_id: string;
  post_id: number;
  predecessor_post_id?: number;
  viewer_url: string;
  post_url: string;
  browser_launch: BrowserLaunch;
  project_label: string;
  working_directory: string;
}
