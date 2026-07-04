# Saltcorn v2

This document outlines a high-level plan for the design of Saltcorn 2.0, the evolution of Saltcorn, an database application builder for web and mobile apps. The goals for the significant rewrite are outlined below after a statement of the scope

## Scope

A Rust library for a unified catalog and server in a cli command with web UI and APIs for 

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
- message bus which is backed by a driver, with an option to use postgres LISTEN/NOTIFY for a simple bus backed by the exisitng database (and another option built in, using a rust library such as apalis or zeromq) and other drivers for Redis, Kafka etc for scalability. real time chat, live collaboration, cache updates flow over the bus

Auth:

- access control lists/language
- can be an identity provider. Option to enable oauth2 server 
- auth at the level of google auth: notice new devices, email if new device
- authorization backed by row level security if available in the database, or runtime checks

Data:

- able to connect many different databases
- facilitate working with legacy databases. No need to discover databases, as soon as a database is connected its tables are usable
- can work with any database design, e.g. composite primary keys, foreign keys to non-primary key fields
- multiple file stores. some file stores are recognized as git repositories.
- lower level query language: enum-based representation of an sql query. inspired by seaquery but data (array of enum values) instead of fluent function calls.

API:

- high-quality API for applications
- enable REST, GraphQL, gRPC, tRPC, and MCP APIs per-application

Admin UI:

- only a subset of users can log in. The uber-admin can define who has admin access and what they can do
- separate the admin ui for the user serving routes. Admin ui has its own URL (could be a different subdomain, or a url path). But it is the same process
- enable strict content security policy
- much better file manager.
- builder is not built in to the admin ui. This comes with the saltcorn1 views. but the builder 
- much improved table editor. bring in as much functionality from airtable's admin ui as possible. this needs to be 
- to comply with CSP we need a new html generating model that can split out onclick etc handlers into a script file. And also XSS needs to be built in, so html tags need to be represented symbolically with raw string values escaped. 
- we will need to recreate a dynamic form framework. Again the client js for this in saltcorn got messy as it grew. Form framework needs to cover conditional fields (field shown depending on value of other fields), repeated forms (like orderlines on an order), selects where options are populated dynamically (from the server, or from client code) depending on other form values, dynamic attributes or contents depending on other form values.

Copilot:

- builtin copilot (chat) and appconstructor
- generate a SKILL.md file if they want to use an external coding agent.

Application UI:

- applications can be written with modern code front end frameworks like next.js and sveltekit and crossplatform mobile frameworks like react native. Code front ends live in a git repository that is equal to or a subdir in a selected file store, and can be edited in a in-browser editor - ideally VS Code for the web.
- applications can also be built in the saltcorn1 experience which will continue to improve
- MAYBE? it should be possible to mix salcorn1 views/pages with code pages. not sure. its tempting to say that each application should be built with either some framework or salcorn1 views. Perhaps the right thing is that each application has a primary UI handler but can also bring in others.
- enable strict CSP

## Created Entities (created by applciation developer)

Cache: all entities except users, workflow runs and files are cached in memory for performance. When a transaction has modified an entity it should signal to all other connected entities to reload the cache for the changed entities.

Fields: cleaner than saltcorn 1. We confused DB fields and Form fields. There should be interfaces for BaseField (shared properties), DataField (a field in a database table) and FormField (a form field) with options to convert between them.

Table: Every table had a table provider (may be a database driver). Each table has an array of fields, any number of rows, and settings for authorization: in saltcorn 1 we had roles (each user has a role), ownership fields and ownership formulae. 

Workflows: similar to v1. Every workflow is a trigger. Each workflow consists of a number of steps, each of which can read and write to a context for that run.

Workflow runs: similar to v1

Agents: similar to v1. Each agent is a trigger, 

Triggers: triggers can be actions, workflows or agents

File stores: Connect 

Files:

Prdictive models: 

## Code entities (supplied by core or plugins)

Database driver: is instantiated once for connecting to a specific database. must execute queries and database operations and manage tables.

Database driver must be written in Rust. The remaining code entities can be written in any supported language.

Table provider: can provide a virtual table. It will look to the user as if it is a database table with fields and rows. Examples: SQL query, RSS feed, IMAP, instant messaging search. This has to interpret the universal query language. and return the rows corresponding to the query. 

Types: Rich types: types known to saltcorn, with attributes and fieldviews. Foreign types: other types not known. The database driver makes a correspondence between types in its database and rich types. 

Fieldviews: can that can display and possibly edit data types in HTML. Each fieldview can display/edit multiple (at least one) types. some catch-all fieldviews can edit any type.

Actions: an elementary step in a workflow, or can be run alone. Has configuration, as output can write to context

Code adapters: a central facility for code entities to use Javascript or Python. These need to maintain an open interpreter that can be used to execute code. Within that interpreter, the entitites in the catalog need to be available. For javascript, this needs to be compatible with saltcorn v1. the code adapters are initialised as needed, no all instalations need all of them. Code in the guest language can provide any of the other code entitty types (except databse driver). 

Importers and exporters: move table data to/from different formats. 

Model providers:

there are more for saltcorn1 views

Viewpatterns:

## Code guidelines

Principles:

1. simple and clean
2. minimise the number of lines of code in the project. Every line of code is a liability
3. Use abstractions, but avoid making them overly complicated. 
4. Testing. Everything must be covered by integration tests. Where possible also by unit tests. But do not complicate the design in order to increase testability.

### build vs introduce dependency

### code reuse

## Milestones

### 1

Database drivers, tables, fields, users. admin UI for those.

### 2 - Add app 

Pick something like NextJS and make sure an app can be built on this, completely served from our process and able to use database