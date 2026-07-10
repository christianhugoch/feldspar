This contains the work in progress design specification for the next version of saltcorn

Files:

docs/GOALS.md - the goals for the next version
docs/Saltcorn1_description.md - a description of the current version of saltcorn
docs/TECHNICAL_DESIGN.md - a technical design document outlining the implementation plan. This is not set in stone - you can deviate from this plan if a better approach is possible. 
TODO.md - a plan with to-do items
CHANGELOG - a list of changes in reverse chronological order

Standard working mode:

1. Pick the most recent unfinished item on the todo list. You can grep for [ ] and [~]
2. Mark the item as in progess if it is not already do
3. Implement the work, making changes to the repository code
4. Write at least one test to assert that the changes work
5. Describe what you did in the changelog
6. Mark the item as complete
7. Do not commit to git. A human will review the work and commit.