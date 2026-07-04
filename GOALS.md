# Saltcorn v2

This document outlines a high-level plan for the design of Saltcorn 2.0, the evolution of Saltcorn, an database application builder for web and mobile apps. The goals for the significant rewrite are outlined below after a statement of the scope

## Scope

A Rust library, unified catalog and server with web UI and APIs for 

  * relational and relation-like data sources. Can connect to multiple databases and also show other data as if it were a table in a database 
  * A workflow engine with durable workflows. Workflows are based on elementary actions. 
  * AI agents based on skills, which are elementary agent capabilities that can be enabled and configures. E.g. ability to search on a table, an tool based on code
  * triggers: workflows and agents. 
  * file stores: connect multiple file stores 
  * predictive models. different types of models run 
  * users. unified authentication and authorization

Everything is focused around the relational data model. Workflows and actions can be run against rows in a database table.

Support for different ways of creating UIs for these entities: 
  
  * front end code based on React, NextJS, svelte etc. For each of these we provide the frontend api connector (react)
 or database interface (nextjs etc). The rust server process must serve the bundled assets. This is the initial focus.
  * A drag and drop building experience like saltcorn v1.

Backwards compatibility with saltcorn v1 is not a goal. Users will have to restore a backup (no in-place update v1 -> v2) and we will write specific import code for this.

## Goals compared to saltcorn 1

General:

- much more reliable, scalable and flexible than saltcorn v1. clean and simple code and types. Rewriting a core in clean Rust allows us to clean up and take into consideration everything we have learned and then expose a backwards-compatible interface with JavaScript bindings
- polyglot. Everything can be written in many different languages. JavaScript, Rust, Python, Java, C#, Go
- multiple applications per data layer. Each application can have access to a subset of tables, file stores etc. 

Auth:

- access control lists/language
- can be an identity provider. Option to enable oauth2 server 
- auth at the level of google auth: notice new devices, email if new device

Data:

- able to connect many different databases
- facilitate working with legacy databases. No need to discover databases, as soon as a database is connected its tables are usable
- can work with any database design, e.g. composite primary keys, foreign keys to non-primary key fields
- multiple file stores. some file stores are recognized as git repositories.
- lower level query language: enum-based representation of an sql query. inspired by seaquery but data (array of enum values) instead of fluent function calls.

API:

- high-quality API for applications
- enable REST, GraphQL, gRPC, tRPC APIs

Admin UI:

- separate the admin ui for the user serving routes. Admin ui has its own URL (could be a different subdomain, or a url path)
- much better file manager.
- builder is not built in to the admin ui. This comes with the saltcorn1 views. but the builder 
- much improved table editor. bring in as much functionality from airtable's admin ui as possible. this needs to be 
- to comply with CSP we need a new html generating model that can split out onclick etc handlers into a script file. And also XSS needs to be built in, so html tags need to be represented symbolically with raw string values escaped. 
- we will need to recreate a dynamic form framework. Again the client js for this in saltcorn got messy as it grew.

Application UI:

- applications can be written with modern code front end frameworks like next.js and sveltekit and crossplatform mobile frameworks like react native. Code front ends live in a git repository that is equal to or a subdir in a selected file store, and can be edited in a in-browser editor - ideally VS Code for the web.
- applications can also be built in the saltcorn1 experience which will continue to improve
- MAYBE? it should be possible to mix salcorn1 views/pages with code pages. not sure. its tempting to say that each application should be built with either some framework or salcorn1 views. Perhaps the right thing is that each application has a primary UI handler but can also bring in others.
- enable strict CSP

## Created Entities (created by applciation developer)

Cache: all entities except users, workflow runs and files are cached in memory for performance. When a transaction has modified an entity it should signal to all other connected entities to reload the cache for the changed entities.

Fields: cleaner than saltcorn 1. We confused DB fields and Form fields. BaseField, DataField and FormField

Table: Every table had a table provider (may be a database driver)

Workflows: similar to v1

Workflow runs: similar to v1

Agents: similar to v1.

Triggers: triggers can be actions, workflows or agents

File stores: Connect 

Files:

Prdictive models: 

## Code entities (supplied by core or plugins)

Database driver:

Table provider: can provide a virtual table. It will look to the user as if it is a database table with fields and rows. Examples: SQL query, RSS feed, IMAP, instant messaging search. This has to interpret the universal query language. and return the rows corresponding to the query

Types: Native types: types known to saltcorn, with attributes and fieldviews. Foreign types: other types not known.

Actions: an elementary step in a workflow, or can be run alone.


Code adapters: a central facility for code entities to use Javascript or Python. These need to maintain an open interpreter that can be used to execute code. Within that interpreter, the entitites in the catalog need to be available. For javascript, this needs to be compatible with saltcorn v1. the code adapters are initialised as needed, no all instalations need all of them. Code in the guest language can provide any of the other code entitty types (except databse driver). 

Importers and exporters: move table data to/from different formats. 

Model providers:

there are more for saltcorn1 views

Fieldviews: similar to Saltcorn, but clean up code interface.
Viewpatterns: