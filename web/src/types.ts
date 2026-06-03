/**
 * API types shared between frontend and server.
 */

export interface Paper {
  dblp_key: string;
  venue: string;
  year: number;
  title: string;
  authors: string;
  doi?: string;
  url?: string;
  abstract?: string;
}

export interface SearchRequest {
  query?: string;
  venue?: string[];
  year?: string;
  rank?: string[];
  tag?: string[];
  sort?: 'relevance' | 'year' | 'venue';
  limit?: number;
  offset?: number;
}

export interface SearchResponse {
  papers: Paper[];
  total?: number;
  offset?: number;
}
