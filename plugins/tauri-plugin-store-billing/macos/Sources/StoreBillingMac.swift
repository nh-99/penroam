// StoreKit 2 for Penroam's subscriptions in the Mac App Store build. It does the same five things
// as the iOS plugin (`ios/Sources/StoreBillingPlugin/StoreBillingPlugin.swift`) and answers with
// the same JSON, but through a C function instead of Tauri's mobile plugin bridge, which macOS
// doesn't have.
//
// Like the iOS side, this never finishes a transaction on its own: `finish` runs only when Rust
// says the sync server has the purchase. An unfinished transaction comes back through
// `Transaction.unfinished`, which is what makes a crash or an outage during a purchase safe.
//
// Calls block the calling thread until StoreKit answers. Rust calls them off the main thread, so
// the main run loop stays free to show the purchase sheet.
import Foundation
import StoreKit

private struct ProductsArgs: Decodable { let ids: [String] }
private struct PurchaseArgs: Decodable {
    let productId: String
    let accountToken: String
}
private struct FinishArgs: Decodable { let transaction: String }

private struct ProductInfo: Encodable {
    let id: String
    let displayPrice: String
    let period: String?
}
private struct ProductList: Encodable { let products: [ProductInfo] }
private struct PurchaseAnswer: Encodable {
    let status: String
    let transaction: String?
}
private struct TransactionList: Encodable { let transactions: [String] }
private struct Empty: Encodable {}

/// The envelope Rust reads: `{"ok": …}` or `{"error": "…"}`.
private struct Ok<T: Encodable>: Encodable { let ok: T }
private struct Failure: Encodable { let error: String }

private struct BillingError: Error, LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

private func period(_ product: Product) -> String? {
    guard let p = product.subscription?.subscriptionPeriod, p.value == 1 else { return nil }
    switch p.unit {
    case .day: return "day"
    case .week: return "week"
    case .month: return "month"
    case .year: return "year"
    @unknown default: return nil
    }
}

/// Transactions whose signature StoreKit verified. The server checks with Apple anyway, so an
/// unverified one is only skipped, not treated as an attack.
private func verified(_ source: Transaction.Transactions) async -> [Transaction] {
    var found: [Transaction] = []
    for await result in source {
        if case .verified(let transaction) = result, transaction.productType == .autoRenewable {
            found.append(transaction)
        }
    }
    return found
}

private func encode<T: Encodable>(_ value: T) -> String {
    let data = (try? JSONEncoder().encode(Ok(ok: value))) ?? Data()
    return String(decoding: data, as: UTF8.self)
}

private func failure(_ message: String) -> String {
    let data = (try? JSONEncoder().encode(Failure(error: message))) ?? Data()
    return String(decoding: data, as: UTF8.self)
}

private func run(_ method: String, _ args: Data) async -> String {
    do {
        let decoder = JSONDecoder()
        switch method {
        case "products":
            let a = try decoder.decode(ProductsArgs.self, from: args)
            let found = try await Product.products(for: a.ids)
            return encode(ProductList(products: found.map {
                ProductInfo(id: $0.id, displayPrice: $0.displayPrice, period: period($0))
            }))
        case "purchase":
            let a = try decoder.decode(PurchaseArgs.self, from: args)
            guard let product = try await Product.products(for: [a.productId]).first else {
                throw BillingError(message: "This plan isn't available in the App Store right now.")
            }
            guard let token = UUID(uuidString: a.accountToken) else {
                throw BillingError(message: "This account can't buy a plan right now.")
            }
            switch try await product.purchase(options: [.appAccountToken(token)]) {
            case .success(.verified(let transaction)):
                return encode(PurchaseAnswer(status: "purchased", transaction: String(transaction.id)))
            case .success(.unverified):
                throw BillingError(message: "The App Store couldn't confirm this purchase.")
            case .pending:
                return encode(PurchaseAnswer(status: "pending", transaction: nil))
            case .userCancelled:
                return encode(PurchaseAnswer(status: "cancelled", transaction: nil))
            @unknown default:
                throw BillingError(message: "The App Store gave an unexpected answer.")
            }
        case "unfinished":
            let found = await verified(Transaction.unfinished)
            return encode(TransactionList(transactions: found.map { String($0.id) }))
        case "restore":
            try await AppStore.sync()
            let found = await verified(Transaction.currentEntitlements)
            return encode(TransactionList(transactions: found.map { String($0.id) }))
        case "finish":
            let a = try decoder.decode(FinishArgs.self, from: args)
            for transaction in await verified(Transaction.unfinished)
            where String(transaction.id) == a.transaction {
                await transaction.finish()
            }
            return encode(Empty())
        default:
            return failure("unknown store method \(method)")
        }
    } catch {
        return failure(error.localizedDescription)
    }
}

/// Holds the answer across the hop from the StoreKit task back to the blocked caller. The
/// semaphore orders the write before the read.
private final class Answer: @unchecked Sendable {
    var text = ""
}

private var updates: Task<Void, Never>?

/// Starts listening for transactions, as StoreKit expects from launch. Transactions that arrive
/// here stay unfinished, and the app picks them up through `unfinished`.
@_cdecl("penroam_store_billing_start")
public func penroamStoreBillingStart() {
    if updates == nil {
        updates = Task.detached {
            for await _ in Transaction.updates {}
        }
    }
}

/// Runs one store method with JSON arguments and returns a JSON envelope. The caller frees the
/// returned string with `free`.
@_cdecl("penroam_store_billing_call")
public func penroamStoreBillingCall(
    _ method: UnsafePointer<CChar>, _ args: UnsafePointer<CChar>
) -> UnsafeMutablePointer<CChar>? {
    let m = String(cString: method)
    let a = Data(String(cString: args).utf8)
    let answer = Answer()
    let done = DispatchSemaphore(value: 0)
    Task.detached {
        answer.text = await run(m, a)
        done.signal()
    }
    done.wait()
    return strdup(answer.text)
}
