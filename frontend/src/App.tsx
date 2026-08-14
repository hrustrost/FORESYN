const foundations = [
  'Verifiable on-chain settlement',
  'Replayable off-chain projections',
  'Small, explicit trust boundaries',
]

function App() {
  return (
    <main className="shell">
      <section className="hero" aria-labelledby="page-title">
        <p className="eyebrow">Foundation milestone</p>
        <h1 id="page-title">Foresyn</h1>
        <p className="summary">
          A production-minded prediction-market prototype focused on secure
          settlement and reliable blockchain indexing.
        </p>

        <ul className="principles">
          {foundations.map((foundation) => (
            <li key={foundation}>{foundation}</li>
          ))}
        </ul>

        <p className="status">
          Contracts, markets, and wallet integration are intentionally not built
          yet.
        </p>
      </section>
    </main>
  )
}

export default App

