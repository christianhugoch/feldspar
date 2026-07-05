This is a description of Saltcorn v1.X, provided to guide the design of the next version.

## Entity types

Saltcorn applications contain the following entity types:

* Tables: These are relational database tables and consist of fields of specified types
    and rows with a value for each field. Fields optionally can be required and/or unique.
    Every field has a name, which is an identifier that is valid in both JavaScript and SQL,
    and a label, which is any short user-friendly string. Every table has a primary key
    (composite primary keys are not supported) which by default is an auto-incrementing integer
    with name `id` and label ID. The `id` primary key field is always unique and not-null by
    definition — never set unique=true or not_null=true on it. Fields can also be of Key type
    (foreign key) referencing a primary key in another table, or its own table for a self-join.
    Tables can have calculated fields, which can be stored or non-stored. Both stored and
    non-stored fields are defined by a JavaScript expression, but only stored fields can
    reference other tables with join fields and aggregations.

* Views: Views are elementary user interfaces into a database table. A view is defined by
    applying a view template (also sometimes called a view pattern, the two are synonymous) to
    a table with a certain configuration. The view template defines the fundamental relationship
    between the UI and the table. For instance, the Show view template displays a single database
    row, the Edit view template is a form that can create a new row or edit an existing row, the
    List view template displays multiple rows in a grid. Views can embed views, for instance Show
    can embed another row through a Key field relationship, or some views are defined by an
    underlying view. For instance, the Feed view repeats an underlying view for multiple tables.
    New viewtemplates are provided by plugin modules.

* Triggers: Triggers connect elementary actions (provided by plugin modules) to either a
    button in the user interface, or a periodic (hourly, daily etc) or table (for instance insert
    on specific table) event. The elementary action each has a number of configuration fields
    that must be filled in after connecting the action to an event, table or button.

* Page: A page has static content but can also embed views for dynamic content. Pages can
    be either defined by a Saltcorn layout, for pages that can be edited with drag and drop, or
    by HTML for more flexible graphic designs. HTML pages should be used for landing pages.

* Plugin modules: plugin modules can supply new field types, view templates or actions.
    Before they can be used, they need to be installed. A plugin may also have a configuration
    that sets options for that plugin. Layout themes in Saltcorn are plugin modules.

## Authorization

Each user in saltcorn had a role set by the user's role_id 1-100. Lower roles are more powerful with 1 being the admin role 
and 100 being the role of unauthenticated user (public). Tables, views and pages have a "minimal role" to access/read/write 1-100 and the user's 
role_id has to be less than or this minimal role to access.

Tables can have ownership by field (key to user) or formula. If this is satisfied, the user can access the row even if they do not meet the minimal 
role to read or write. But if they do meet the minimal role criteria for the table as a whole, they can access all rows. Therefore a user who has a role_id less than or equal to the minimum role to read (or write) can read (or write, respectively) all roles even if ownership is set. Ownership only determines access for users with a role_id greater than the minumum role to read (or write).

## Resources

Code: https://github.com/saltcorn/saltcorn
Wiki: https://wiki.saltcorn.com/
